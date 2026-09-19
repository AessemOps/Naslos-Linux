package server

import (
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/http"
	"regexp"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
	"github.com/AessemOps/Naslos-Linux/api/internal/logsafe"
)

// ZFS Pool API handlers.
// Pool operations are executed by the privileged naslos-agent DaemonSet
// running on each node, since ZFS pools live outside Talos's volume system.

// poolNamePattern is the ZFS-safe charset: must start alphanumerically.
var poolNamePattern = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9._:-]*$`)

// validTopologies are the topologies the agent's CreatePool understands.
var validTopologies = map[string]bool{
	"": true, "single": true, "mirror": true,
	"raidz": true, "raidz1": true, "raidz2": true, "raidz3": true,
}

func (s *Server) handleZFSPools(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		pools, err := s.agent.ListPools()
		if err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, pools)
	case http.MethodPost:
		// Create ZFS pool
		var req struct {
			Name     string            `json:"name"`
			Topology string            `json:"topology"` // mirror, raidz1, raidz2, raidz3
			Disks    []string          `json:"disks"`
			Cache    string            `json:"cache"` // optional L2ARC device
			Options  map[string]string `json:"options"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		// Validate before forwarding so malformed requests fail fast with
		// 400 even when the agent is unreachable or degraded (503).
		if !poolNamePattern.MatchString(req.Name) {
			writeError(w, http.StatusBadRequest,
				fmt.Sprintf("invalid pool name %q: must start with a letter or digit and contain only letters, digits, '.', '_', ':' or '-'", req.Name))
			return
		}
		if len(req.Disks) == 0 {
			writeError(w, http.StatusBadRequest, "at least one disk is required")
			return
		}
		if !validTopologies[strings.ToLower(strings.TrimSpace(req.Topology))] {
			writeError(w, http.StatusBadRequest,
				fmt.Sprintf("unsupported topology %q (supported: single, mirror, raidz1, raidz2, raidz3)", req.Topology))
			return
		}
		// Check the disks against the node before creating anything: a bad,
		// duplicated or already-attached disk must be a fast 400 rather than a
		// rejected agent call (the agent validates too - defense in depth).
		if err := s.validateAddDisks(req.Topology, req.Disks); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if req.Cache != "" {
			if err := s.validateAddDisks("single", []string{req.Cache}); err != nil {
				writeError(w, http.StatusBadRequest, "cache device: "+err.Error())
				return
			}
			for _, disk := range req.Disks {
				if strings.TrimSpace(disk) == strings.TrimSpace(req.Cache) {
					writeError(w, http.StatusBadRequest, "the cache device cannot also be a data disk")
					return
				}
			}
		}
		if err := s.agent.CreatePool(agent.CreatePoolRequest{
			Name:     req.Name,
			Topology: req.Topology,
			Disks:    req.Disks,
			Cache:    req.Cache,
			Options:  req.Options,
		}); err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{
			"status":   "pool created",
			"pool":     req.Name,
			"topology": req.Topology,
		})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleZFSImport handles pool import: GET lists importable pools, POST imports.
func (s *Server) handleZFSImport(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		pools, err := s.agent.ListImportablePools()
		if err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, pools)
	case http.MethodPost:
		var req struct {
			Name string `json:"name"`
		}
		json.NewDecoder(r.Body).Decode(&req)
		// An empty name means "import everything"; a named pool must be a real
		// pool name before it reaches the agent.
		if req.Name != "" && !poolNamePattern.MatchString(req.Name) {
			writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid pool name %q", req.Name))
			return
		}
		if err := s.agent.ImportPool(req.Name); err != nil {
			writeAgentError(w, err)
			return
		}
		if req.Name == "" {
			writeJSON(w, http.StatusOK, map[string]string{"status": "all pools imported"})
		} else {
			writeJSON(w, http.StatusOK, map[string]string{"status": "pool imported", "pool": req.Name})
		}
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleZFSPoolDetail handles per-pool operations: GET status, GET health, DELETE pool.
// Routes: GET /api/volumes/zfs/{name}      → raw zpool status
//
//	GET /api/volumes/zfs/{name}/health → structured health data
//	DELETE /api/volumes/zfs/{name}   → destroy pool
func (s *Server) handleZFSPoolDetail(w http.ResponseWriter, r *http.Request) {
	name := strings.TrimPrefix(r.URL.Path, "/api/volumes/zfs/")
	if name == "" {
		writeError(w, http.StatusBadRequest, "pool name required")
		return
	}
	// Dispatch /health and /devices sub-routes.
	if strings.HasSuffix(name, "/health") {
		s.handleZFSPoolHealth(w, r, strings.TrimSuffix(name, "/health"))
		return
	}
	if strings.HasSuffix(name, "/devices") {
		s.handleZFSPoolDevices(w, r, strings.TrimSuffix(name, "/devices"))
		return
	}
	if strings.Contains(name, "/") {
		writeError(w, http.StatusBadRequest, "pool name required")
		return
	}
	// Mirror the agent's own check so a malformed name is a 400 here rather than
	// a rejected agent call (the agent validates too - this is defense in depth).
	if !poolNamePattern.MatchString(name) {
		writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid pool name %q", name))
		return
	}
	switch r.Method {
	case http.MethodGet:
		health, err := s.agent.PoolHealth(name)
		if err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, health)
	case http.MethodDelete:
		if err := s.agent.DeletePool(name); err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "pool destroyed", "pool": name})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// datasetNamePattern is the ZFS-safe charset for each component of a dataset
// name, mirroring the agent's check so bad input fails fast with a 400.
var datasetNamePattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]*$`)

// datasetCompression are the algorithms the dataset form may set.
var datasetCompression = map[string]bool{
	"on": true, "off": true, "lz4": true, "zstd": true, "zstd-fast": true,
	"gzip": true, "gzip-1": true, "gzip-9": true, "lzjb": true, "zle": true,
}

// sizePattern matches ZFS sizes such as 500G or 1.5T.
var sizePattern = regexp.MustCompile(`^[0-9]+(\.[0-9]+)?[KMGTPE]?$`)

// vdevMinimumDisks mirrors the agent's rule (ZFS's own minimum per topology) so
// the user gets an immediate answer instead of a round trip.
var vdevMinimumDisks = map[string]int{
	"": 1, "single": 1, "stripe": 1, "mirror": 2,
	"raidz": 2, "raidz1": 2, "raidz2": 3, "raidz3": 4,
}

// handleDatasets lists, creates and destroys datasets.
//
//	GET    /api/datasets                     → every dataset, with space used
//	POST   /api/datasets {pool,name,options} → create
//	DELETE /api/datasets?name=<pool>/<name>  → destroy (recursive=true to force)
//
// A dataset is the level a share points at, so this is what turns a pool into
// something useful without shelling into the node.
func (s *Server) handleDatasets(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		datasets, err := s.agent.ListDatasets()
		if err != nil {
			writeAgentError(w, err)
			return
		}
		writeJSON(w, http.StatusOK, datasets)

	case http.MethodPost:
		var req struct {
			Pool    string            `json:"pool"`
			Name    string            `json:"name"`
			Options map[string]string `json:"options"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if err := validateDatasetCreate(req.Pool, req.Name, req.Options); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		// The pool has to exist: otherwise a typo is reported as a ZFS error
		// after the dataset name was already accepted.
		if err := s.requirePool(req.Pool); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		if err := s.agent.CreateDataset(req.Pool, req.Name, req.Options); err != nil {
			writeAgentError(w, err)
			return
		}
		log.Printf("created dataset %s/%s", req.Pool, req.Name)
		writeJSON(w, http.StatusCreated, map[string]string{
			"status": "dataset created",
			"name":   req.Pool + "/" + req.Name,
		})

	case http.MethodDelete:
		name := strings.TrimSpace(r.URL.Query().Get("name"))
		recursive := r.URL.Query().Get("recursive") == "true"
		if err := validateDatasetPath(name); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		// A dataset that is serving a share must not vanish underneath it.
		if err := s.requireUnusedByShares(name); err != nil {
			writeError(w, http.StatusConflict, err.Error())
			return
		}
		if err := s.agent.DestroyDataset(name, recursive); err != nil {
			writeAgentError(w, err)
			return
		}
		log.Printf("destroyed dataset %s (recursive=%v)", logsafe.Field(name), recursive) // #nosec G706 -- sanitised by logsafe.Field
		writeJSON(w, http.StatusOK, map[string]string{"status": "dataset destroyed", "name": name})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// requirePool confirms a pool exists, so dependent operations report the problem
// instead of letting ZFS complain about a missing pool.
func (s *Server) requirePool(name string) error {
	pools, err := s.agent.ListPools()
	if err != nil {
		return fmt.Errorf("cannot verify pool %s (agent unavailable): %w", name, err)
	}
	for _, p := range pools {
		if p.Name == name {
			return nil
		}
	}
	return fmt.Errorf("pool %q not found", name)
}

// requireUnusedByShares refuses to destroy a dataset that a share serves, or a
// parent of one: the share would keep working until the next restart and then
// fail, with the data already gone. Share paths are mount paths, so the check is
// made against the mountpoints of the dataset and anything below it.
func (s *Server) requireUnusedByShares(name string) error {
	datasets, err := s.agent.ListDatasets()
	if err != nil {
		return fmt.Errorf("cannot verify which paths dataset %s backs: %w", name, err)
	}

	mountpoints := make([]string, 0, 2)
	for _, d := range datasets {
		if d.Name == name || strings.HasPrefix(d.Name, name+"/") {
			if d.Mountpoint != "" && d.Mountpoint != "none" {
				mountpoints = append(mountpoints, d.Mountpoint)
			}
		}
	}

	for _, share := range s.shares.List() {
		for _, mp := range mountpoints {
			if share.Path == mp || strings.HasPrefix(share.Path, mp+"/") {
				return fmt.Errorf("dataset %s backs path %s, which share %q is serving: delete or repoint the share first", name, mp, share.Name)
			}
		}
	}
	return nil
}

// validateDatasetCreate checks a create request: pool name, dataset name and the
// safe option subset.
func validateDatasetCreate(pool, name string, options map[string]string) error {
	if !poolNamePattern.MatchString(pool) {
		return fmt.Errorf("invalid pool name %q", pool)
	}
	if err := validateDatasetName(name); err != nil {
		return err
	}
	for key, value := range options {
		value = strings.TrimSpace(value)
		switch key {
		case "compression":
			if !datasetCompression[value] {
				return fmt.Errorf("unsupported compression %q", value)
			}
		case "quota":
			if value != "" && value != "none" && !sizePattern.MatchString(value) {
				return fmt.Errorf("invalid quota %q: use a size such as 500G, or none", value)
			}
		case "recordsize":
			if !sizePattern.MatchString(value) {
				return fmt.Errorf("invalid recordsize %q", value)
			}
		case "atime", "readonly":
			if value != "on" && value != "off" {
				return fmt.Errorf("%s must be on or off", key)
			}
		case "copies":
			if value != "1" && value != "2" && value != "3" {
				return fmt.Errorf("copies must be 1, 2 or 3")
			}
		default:
			return fmt.Errorf("unsupported dataset option %q", key)
		}
	}
	return nil
}

// validateDatasetName checks a dataset name relative to its pool.
func validateDatasetName(name string) error {
	trimmed := strings.TrimSpace(name)
	if trimmed == "" {
		return fmt.Errorf("dataset name is required")
	}
	if trimmed != name {
		return fmt.Errorf("dataset name must not start or end with whitespace")
	}
	if strings.HasPrefix(trimmed, "/") {
		return fmt.Errorf("dataset name must be relative to its pool, not a path")
	}
	if strings.Contains(trimmed, "@") {
		return fmt.Errorf("dataset name must not contain '@' (that is a snapshot)")
	}
	for _, part := range strings.Split(trimmed, "/") {
		if !datasetNamePattern.MatchString(part) {
			return fmt.Errorf("invalid dataset name %q: each part must start with a letter or digit and contain only letters, digits, '.', '_' or '-'", trimmed)
		}
	}
	return nil
}

// validateDatasetPath checks a full dataset path (pool/name[/…]).
func validateDatasetPath(name string) error {
	if name == "" {
		return fmt.Errorf("dataset name is required")
	}
	parts := strings.Split(name, "/")
	if len(parts) < 2 {
		return fmt.Errorf("dataset %q must be <pool>/<name>", name)
	}
	if !poolNamePattern.MatchString(parts[0]) {
		return fmt.Errorf("invalid pool name %q", parts[0])
	}
	return validateDatasetName(strings.Join(parts[1:], "/"))
}

// handleZFSPoolDevices attaches disks to an existing pool: this is how a pool
// grows. It is the most consequential operation in the product - it writes to
// raw disks, and adding a redundancy-less vdev also lowers the pool's fault
// tolerance - so it is guarded on every side and needs explicit confirmation in
// the UI.
func (s *Server) handleZFSPoolDevices(w http.ResponseWriter, r *http.Request, pool string) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if !poolNamePattern.MatchString(pool) {
		writeError(w, http.StatusBadRequest, "invalid pool name")
		return
	}

	var req struct {
		Disks    []string `json:"disks"`
		Topology string   `json:"topology"`
		Force    bool     `json:"force"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	if err := s.requirePool(pool); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	if err := s.validateAddDisks(req.Topology, req.Disks); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	if err := s.agent.AddPoolVDev(pool, req.Topology, req.Disks, req.Force); err != nil {
		writeAgentError(w, err)
		return
	}
	log.Printf("attached %d disk(s) to pool %s (topology %q, force %v)", // #nosec G706 -- arguments sanitised by logsafe.Field
		len(req.Disks), logsafe.Field(pool), logsafe.Field(req.Topology), req.Force)
	writeJSON(w, http.StatusCreated, map[string]interface{}{
		"status":   "vdev added",
		"pool":     pool,
		"topology": req.Topology,
		"disks":    req.Disks,
	})
}

// validateAddDisks checks an add-vdev request against the node's real inventory:
// only disks the node itself reported (which excludes the system disk), and only
// disks that no pool already owns.
func (s *Server) validateAddDisks(topology string, disks []string) error {
	norm := strings.ToLower(strings.TrimSpace(topology))
	minimum, known := vdevMinimumDisks[norm]
	if !known {
		return fmt.Errorf("unsupported topology %q (supported: single, mirror, raidz1, raidz2, raidz3)", topology)
	}
	if len(disks) == 0 {
		return fmt.Errorf("select at least one disk")
	}
	if len(disks) < minimum {
		return fmt.Errorf("topology %q needs at least %d disks, got %d", norm, minimum, len(disks))
	}

	discovered, err := s.talos.GetDiscoveredVolumes()
	if err != nil {
		return fmt.Errorf("cannot list the node's disks: %w", err)
	}
	usable := make(map[string]bool, len(discovered))
	for _, d := range discovered {
		if d.SystemDisk {
			continue
		}
		usable[d.DeviceName] = true
	}

	// A disk already in a pool must not be attached again: `zpool add -f` would
	// overwrite the owning pool's label and destroy it.
	inPool := map[string]string{}
	if pools, err := s.agent.ListPools(); err == nil {
		for _, p := range pools {
			for _, d := range p.Disks {
				inPool[d] = p.Name
			}
		}
	}

	return validateDiskSelection(disks, usable, inPool)
}

// validateDiskSelection is the pure part of the disk check: absolute /dev paths,
// no duplicates, no disk another pool owns, and only disks the node can use.
func validateDiskSelection(disks []string, usable map[string]bool, inPool map[string]string) error {
	seen := map[string]bool{}
	for _, disk := range disks {
		dev := strings.TrimSpace(disk)
		if !strings.HasPrefix(dev, "/dev/") {
			return fmt.Errorf("disk %q must be an absolute path under /dev", disk)
		}
		if seen[dev] {
			return fmt.Errorf("disk %s was selected twice", dev)
		}
		seen[dev] = true
		if owner, inUse := inPool[dev]; inUse {
			return fmt.Errorf("disk %s already belongs to pool %q", dev, owner)
		}
		if !usable[dev] {
			return fmt.Errorf("disk %s is not a disk this node can use (unknown, or the system disk)", dev)
		}
	}
	return nil
}

// handleZFSPoolHealth returns structured health data (device tree, IO stats,
// scan state, errors) for a pool by name.
func (s *Server) handleZFSPoolHealth(w http.ResponseWriter, r *http.Request, name string) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if !poolNamePattern.MatchString(name) {
		writeError(w, http.StatusBadRequest, "invalid pool name")
		return
	}
	health, err := s.agent.PoolHealth(name)
	if err != nil {
		writeAgentError(w, err)
		return
	}
	writeJSON(w, http.StatusOK, health)
}

// writeAgentError maps agent client errors to HTTP responses. Agent-side
// failures (validation, degraded mode, zpool errors) carry their upstream
// status — notably 503 when the agent runs degraded (no ZFS on the host) —
// so the UI can distinguish "no ZFS here" from a real server error.
// Transport failures (agent unreachable) become 502.
func writeAgentError(w http.ResponseWriter, err error) {
	var agentErr *agent.Error
	if errors.As(err, &agentErr) && agentErr.Status != 0 {
		writeError(w, agentErr.Status, agentErr.Message)
		return
	}
	writeError(w, http.StatusBadGateway, err.Error())
}
