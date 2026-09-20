package server

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

// Instance-side sender and restore (FR-BUD-11…14).
//
// Up to here an instance could only *receive* pushes; sending required an operator
// with `buddyctl` on the box. These handlers close that gap by combining the two
// halves the API already owns: the agent (which can stream `zfs send`/`zfs receive`
// on the host) and the buddy client (which encrypts and uploads). Nothing is
// buffered, so a multi-terabyte dataset is never held in memory, and the stream is
// checked against the agent's own estimate before the manifest is published.

// defaultBuddyIdentity is where this instance's key material lives. It sits beside
// the other Naslos state files, on the persistent volume.
const defaultBuddyIdentity = "/var/lib/naslos/buddy-identity.json"

// buddyIdentityPath is the configured identity file.
func buddyIdentityPath() string {
	if env := os.Getenv("BUDDY_IDENTITY"); env != "" {
		return env
	}
	return defaultBuddyIdentity
}

// loadBuddyIdentity reads this instance's identity, or explains how to make one.
func loadBuddyIdentity() (*buddy.Identity, error) {
	path := buddyIdentityPath()
	identity, err := buddy.LoadIdentity(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("this instance has no backup identity yet: create one with POST /api/buddy/identity " +
				"(its public key is what a buddy authorizes)")
		}
		return nil, err
	}
	return identity, nil
}

// handleBuddyIdentity reports this instance's public key, or creates the keypair on
// first use:
//
//	GET  /api/buddy/identity          → who this instance is to its buddies
//	POST /api/buddy/identity {name, replace}
//
// The private key and the key encryption key never leave the file and are never
// returned; the public key is what an operator hands to a buddy.
func (s *Server) handleBuddyIdentity(w http.ResponseWriter, req *http.Request) {

	path := buddyIdentityPath()
	switch req.Method {
	case http.MethodGet:
		identity, err := buddy.LoadIdentity(path)
		if err != nil {
			if os.IsNotExist(err) {
				writeJSON(w, http.StatusOK, map[string]any{
					"exists": false,
					"path":   path,
					"help":   "create one with POST /api/buddy/identity; back the file up, losing it means losing the ability to read your backups",
				})
				return
			}
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		fingerprint, err := identity.Fingerprint()
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{
			"exists":      true,
			"name":        identity.Name,
			"publicKey":   identity.PublicKey,
			"fingerprint": fingerprint,
			"path":        path,
			"createdAt":   identity.CreatedAt,
		})

	case http.MethodPost:
		var request struct {
			Name    string `json:"name"`
			Replace bool   `json:"replace"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil && err != io.EOF {
			writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
			return
		}

		if _, err := os.Stat(path); err == nil && !request.Replace {
			// Replacing an identity orphans every backup this instance ever made
			// (the data keys can no longer be unwrapped), so it takes an explicit
			// flag - and the caller is told what it costs.
			writeError(w, http.StatusConflict,
				"an identity already exists at "+path+": it is the only key that can read this instance's backups, "+
					"so replacing it loses them. Send {\"replace\": true} if that is really what you want.")
			return
		}

		name := strings.TrimSpace(request.Name)
		if name == "" {
			host, _ := os.Hostname()
			name = strings.ToLower(strings.TrimSpace(host))
		}
		if name == "" {
			name = "naslos"
		}

		identity, err := buddy.NewIdentity(name)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		if err := identity.Save(path); err != nil {
			writeError(w, http.StatusInternalServerError, "saving the identity: "+err.Error())
			return
		}

		fingerprint, err := identity.Fingerprint()
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		log.Printf("buddy: created a backup identity for %q (%s) at %s", identity.Name, fingerprint, path)
		writeJSON(w, http.StatusCreated, map[string]any{
			"exists":      true,
			"name":        identity.Name,
			"publicKey":   identity.PublicKey,
			"fingerprint": fingerprint,
			"path":        path,
			"warning":     "back this file up somewhere your buddies cannot reach: it holds the only key that can read your backups",
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// sendShortfallAllowance is how far below the dry run's estimate a complete stream
// may land, before we call the stream truncated.
//
// `zfs send -nP` reports the payload, not the stream's framing records, and the
// relationship is not a strict floor: measured on the VM, a full send of 44,376 bytes
// came in 232 bytes *above* its estimate while a 53 MB full send was 9,712 above, but
// an incremental send of 56.9 MB came in 119,688 bytes *below* it. So the estimate is
// an approximation, and the allowance has to tolerate a percent in either direction.
//
// What this guard is for: a `zfs send` that died loses everything after the kill, so
// the shortfall is normally huge. Anything smaller is caught where it matters - ZFS
// refuses a truncated stream at `zfs receive` (the stream carries its own end record
// and per-record checksums) - and the sender can simply retry, which resumes the
// chain. It is an early warning, not the integrity boundary.
func sendShortfallAllowance(estimate int64) int64 {
	const floor = 1 << 20 // 1 MiB
	relative := estimate / 100
	if relative > floor {
		return relative
	}
	return floor
}

// instanceSendState is what an interrupted instance-side send needs to continue: the
// buddy chain (its data key and nonce prefix) plus the snapshot pair and GUIDs the
// stream was made from. Keeping all of it means a retry produces the *same* bytes,
// so the buddy skips what already arrived instead of storing a second copy.
type instanceSendState struct {
	Chain        buddy.ChainState `json:"chain"`
	Receiver     string           `json:"receiver"`
	Source       string           `json:"source"`
	Dataset      string           `json:"dataset"`
	ToSnapshot   string           `json:"toSnapshot"`
	FromSnapshot string           `json:"fromSnapshot"`
	FromGUID     string           `json:"fromGUID"`
	ToGUID       string           `json:"toGUID"`
}

// randomSuffix returns four hex characters so two sends started in the same second
// still get distinct snapshot names.
func randomSuffix() string {
	buf := make([]byte, 2)
	if _, err := rand.Read(buf); err != nil {
		return fmt.Sprintf("%04x", time.Now().UnixNano()&0xffff)
	}
	return hex.EncodeToString(buf)
}

// sendStateDir is where interrupted sends keep their state, beside the identity.
func sendStateDir() string {
	return filepath.Join(filepath.Dir(buddyIdentityPath()), "buddy-sends")
}

// sendStateName names the state file for a (buddy, source) pair.
func sendStateName(receiverURL, source string) string {
	sum := sha256.Sum256([]byte(receiverURL + "|" + source))
	return hex.EncodeToString(sum[:16]) + ".json"
}

// saveSendState writes the resume state with owner-only permissions: it carries a
// chain data key.
func saveSendState(path string, state *instanceSendState) error {
	data, err := json.MarshalIndent(state, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".send-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if err := tmp.Chmod(0600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}

// loadSendState reads a resume state, if one exists.
func loadSendState(path string) (*instanceSendState, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var state instanceSendState
	if err := json.Unmarshal(data, &state); err != nil {
		return nil, err
	}
	return &state, nil
}

type buddySendRequest struct {
	// Dataset is the local ZFS dataset to back up.
	Dataset string `json:"dataset"`
	// Source is the name it is stored under on the buddy.
	Source string `json:"source"`
	// Receiver is the buddy's base URL.
	Receiver string `json:"receiver"`
	// PruneKeep, when set, keeps only the newest N chains on the buddy.
	PruneKeep int `json:"pruneKeep"`
	// Raw sends the encrypted records (-w). Default true: the buddy should never
	// need to decrypt anything.
	Raw *bool `json:"raw"`
	// Force sends a full stream even when an incremental base is available.
	Force bool `json:"force"`
}

// normalizeReceiverURL validates a buddy URL. Peers are reached over the network
// the operator already exposes, so anything but http/https is a mistake worth
// refusing here rather than inside the client.
func normalizeReceiverURL(raw string) (string, error) {
	trimmed := strings.TrimSpace(raw)
	if trimmed == "" {
		return "", fmt.Errorf("receiver is required: it is the buddy's base URL")
	}
	parsed, err := url.Parse(trimmed)
	if err != nil {
		return "", fmt.Errorf("invalid receiver URL: %w", err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return "", fmt.Errorf("receiver URL %q must start with http:// or https://", trimmed)
	}
	if parsed.Host == "" {
		return "", fmt.Errorf("receiver URL %q has no host", trimmed)
	}
	return strings.TrimRight(trimmed, "/"), nil
}

// datasetMountState reports what the agent knows about a dataset, or nil when it
// cannot be looked up (a listing failure must not become a refusal - the send
// reports the real problem if there is one).
func (s *Server) datasetMountState(dataset string) *agent.Dataset {
	if s.agent == nil {
		return nil
	}
	datasets, err := s.agent.ListDatasets()
	if err != nil {
		log.Printf("buddy send: cannot check the mount state of %s (%v)", dataset, err)
		return nil
	}
	for i := range datasets {
		if datasets[i].Name == dataset {
			return &datasets[i]
		}
	}
	return nil
}

// requireMountedDataset refuses to back up a dataset whose contents `zfs send`
// cannot see. A dataset created from inside a pod is mounted in that pod's mount
// namespace only: in the host namespace its mountpoint is an ordinary directory
// on the parent dataset, so a send captures an empty filesystem while reporting
// success (the mount-propagation trap, seen live as a 44 KB "backup" of a 2 GiB
// dataset). The agent's view is authoritative because that is where `zfs send`
// runs.
//
// A dataset with mountpoint "none"/"legacy" is not affected - there is no
// directory to shadow it - so it stays sendable.
func (s *Server) requireMountedDataset(dataset string) error {
	d := s.datasetMountState(dataset)
	if d == nil {
		return nil
	}
	if !d.Mounted && strings.HasPrefix(d.Mountpoint, "/") {
		return fmt.Errorf("dataset %s is not mounted on the node (mountpoint %s), so zfs send would "+
			"capture an empty filesystem; mount it in the host namespace (reboot the node, or run "+
			"`zfs mount %s` there) and retry", dataset, d.Mountpoint, dataset)
	}
	return nil
}

// requireStreamMatchesDataset is the second half of the mount-propagation guard
// (AV-8). requireMountedDataset catches a dataset the agent cannot see at all;
// this catches the subtler case the drill produced: a dataset that IS mounted in
// the agent's namespace but whose files were written somewhere that namespace
// cannot observe, so the snapshot, the send and the stored stream are all
// internally consistent and tiny.
//
// The signal is the dataset's own reported used space: it counts the blocks that
// exist regardless of who can see the mount, so a stream that is a small
// fraction of it did not capture the contents. A dataset that legitimately
// compresses heavily, or that is genuinely nearly empty, is not flagged because
// the check only fires when the dataset reports substantially more used space
// than the stream carried AND that used space is large enough to be real data
// (below the floor, metadata dominates and the ratio is meaningless).
func requireStreamMatchesDataset(dataset string, usedBytes, plainBytes int64) error {
	const (
		// Below this the dataset is effectively empty and the used figure is
		// almost all metadata, so a small stream is expected.
		emptyFloor = 1 << 20 // 1 MiB
		// A stream smaller than this fraction of the dataset's used space means
		// the contents did not make it into the stream.
		minRatio = 0.05 // 5%
	)
	if usedBytes < emptyFloor || plainBytes <= 0 {
		return nil
	}
	if float64(plainBytes) < float64(usedBytes)*minRatio {
		return fmt.Errorf("the send captured %d bytes but %s reports %d bytes used, so its contents "+
			"were not included (the dataset is not visible to zfs send in the host mount namespace); "+
			"the backup was not published. Mount the dataset where the agent can see it (reboot the "+
			"node, or `zfs mount` there) and retry", plainBytes, dataset, usedBytes)
	}
	return nil
}

// buddyRestoreRequest is a restore back onto this instance's own ZFS.
type buddyRestoreRequest struct {
	// Source is the backup's logical name on the buddy.
	Source string `json:"source"`
	// Receiver is the buddy's base URL (where the backup lives).
	Receiver string `json:"receiver"`
	// Dataset is the local destination (`pool/name`). Not needed for verify.
	Dataset string `json:"dataset"`
	// Chain restores an older chain instead of the current one.
	Chain string `json:"chain"`
	// Force applies `zfs receive -F`: roll the destination back to the stream's
	// snapshot instead of refusing when it already has one.
	Force bool `json:"force"`
	// Verify decrypts and hashes the stream without touching ZFS: the proof that a
	// backup is intact, and the check a restore drill can compare with the node.
	Verify bool `json:"verify"`
}

// streamDigest is the result of a verify run.
type streamDigest struct {
	Chain  string `json:"chain"`
	SHA256 string `json:"sha256"`
	Bytes  int64  `json:"bytes"`
}

// handleBuddyRestore restores a stored backup onto this instance's node.
//
//	POST /api/buddy/restore {"source":"naslos-a/test","receiver":"https://buddy","dataset":"test/restored"}
//
// The stream is authenticated before ZFS sees it: the buddy client verifies the
// manifest signature, the chain's contiguity and every chunk's digest, and ZFS
// then refuses anything truncated. A restore either lands whole or fails loudly.
func (s *Server) handleBuddyRestore(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.agent == nil {
		writeError(w, http.StatusServiceUnavailable, "the API has no agent client, so it cannot stream ZFS data")
		return
	}

	var request buddyRestoreRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
		return
	}

	source := strings.TrimSpace(request.Source)
	dataset := strings.TrimSpace(request.Dataset)
	if source == "" {
		writeError(w, http.StatusBadRequest, "source is required")
		return
	}
	if dataset == "" && !request.Verify {
		writeError(w, http.StatusBadRequest, "dataset is required (it is where the backup is restored to)")
		return
	}
	if err := buddy.ValidateSource(source); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	receiverURL, err := normalizeReceiverURL(request.Receiver)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	identity, err := loadBuddyIdentity()
	if err != nil {
		writeError(w, http.StatusPreconditionFailed, err.Error())
		return
	}
	client := buddy.NewClient(receiverURL, identity)
	ctx := req.Context()

	// A restore is usually a *sequence*: the newest backup is normally an
	// incremental, and ZFS refuses an incremental stream whose base is missing. So
	// the chains are worked out first (oldest full send, then each incremental) and
	// applied in that order - which is the only order that can succeed.
	sequence, err := client.RestoreSequenceContext(ctx, source, request.Chain)
	if err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}

	started := time.Now()

	// Verify: decrypt and hash every chain of the sequence without touching ZFS.
	// This is the cheap half of a restore drill - and the half that proves the
	// bytes are intact - so it is worth having as an operation of its own.
	if request.Verify {
		digests := make([]streamDigest, 0, len(sequence))
		var total int64
		for _, chain := range sequence {
			hasher := sha256.New()
			result, err := client.Restore(buddy.RestoreOptions{
				Context: ctx,
				Source:  source,
				Chain:   chain.Chain,
				Out:     hasher,
			})
			if err != nil {
				writeError(w, http.StatusBadGateway, err.Error())
				return
			}
			digests = append(digests, streamDigest{
				Chain:  chain.Chain,
				SHA256: hex.EncodeToString(hasher.Sum(nil)),
				Bytes:  result.PlainBytes,
			})
			total += result.PlainBytes
		}

		log.Printf("buddy verify: %s %s (%d chain(s), %d bytes)", receiverURL, source, len(digests), total)
		writeJSON(w, http.StatusOK, map[string]any{
			"status":          "verified",
			"source":          source,
			"receiver":        receiverURL,
			"chains":          digests,
			"plainBytes":      total,
			"durationSeconds": time.Since(started).Seconds(),
		})
		return
	}

	var (
		totalChunks int
		totalBytes  int64
		applied     = make([]string, 0, len(sequence))
	)
	for i, chain := range sequence {
		// -F only on the first stream: later ones are incrementals that must not be
		// forced, or the destination would be rolled back between them.
		force := request.Force && i == 0
		writer, wait, err := s.agent.ReceiveStream(ctx, dataset, force)
		if err != nil {
			writeError(w, http.StatusBadGateway, "starting zfs receive: "+err.Error())
			return
		}

		result, err := client.Restore(buddy.RestoreOptions{
			Context: ctx,
			Source:  source,
			Chain:   chain.Chain,
			Out:     writer,
		})
		if err != nil {
			// Close the agent's stream and collect whatever ZFS said about the
			// partial data; the error from the buddy side is the useful one.
			_ = writer.Close()
			_ = wait()
			writeError(w, http.StatusBadGateway, err.Error())
			return
		}
		// The writer must be closed first: that is what tells `zfs receive` the
		// stream ended, and only then can it finish the dataset.
		if err := wait(); err != nil {
			writeError(w, http.StatusUnprocessableEntity, fmt.Sprintf(
				"zfs receive refused chain %s: %s", chain.Chain, err.Error()))
			return
		}

		applied = append(applied, chain.Chain)
		totalChunks += result.Chunks
		totalBytes += result.PlainBytes
	}

	last := sequence[len(sequence)-1]
	log.Printf("buddy restore: %s %s -> %s (%d chain(s), %d chunks, %d bytes)",
		receiverURL, source, dataset, len(applied), totalChunks, totalBytes)

	writeJSON(w, http.StatusOK, map[string]any{
		"status":          "restored",
		"source":          source,
		"dataset":         dataset,
		"chains":          applied,
		"chain":           last.Chain,
		"incrementalFrom": last.FromSnapshot,
		"snapshot":        last.ToSnapshot,
		"chunks":          totalChunks,
		"plainBytes":      totalBytes,
		"kind":            last.Kind,
		"takenAt":         last.CreatedAt,
		"durationSeconds": time.Since(started).Seconds(),
	})
}
