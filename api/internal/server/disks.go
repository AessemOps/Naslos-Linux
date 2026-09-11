package server

import (
	"encoding/json"
	"net/http"

	"github.com/AessemOps/Naslos-Linux/api/internal/talos"
	"github.com/siderolabs/talos/pkg/machinery/api/storage"
)

// isRealDisk filters out devices that cannot be used for ZFS pools:
// loop devices (UNKNOWN type), optical drives (CD type), and the system disk.
// Real disks are SSD, HDD, NVME, or SD (eMMC / card).
func isRealDisk(d *storage.Disk) bool {
	if d.SystemDisk {
		return false
	}
	switch d.Type {
	case storage.Disk_SSD, storage.Disk_HDD, storage.Disk_NVME, storage.Disk_SD:
		return true
	default:
		return false
	}
}

// handleDisks returns discovered disks suitable for pool creation.
func (s *Server) handleDisks(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	disks, err := s.talos.GetDiscoveredVolumes()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	// Convert to our DiskInfo format, filtering out non-disk devices.
	result := make([]map[string]interface{}, 0, len(disks))
	for _, d := range disks {
		if !isRealDisk(d) {
			continue
		}
		result = append(result, map[string]interface{}{
			"device":        d.DeviceName,
			"size":          d.Size,
			"isSystemDisk":  d.SystemDisk,
			"model":         d.Model,
			"serial":        d.Serial,
			"busPath":       d.BusPath,
			"type":          d.Type.String(),
		})
	}

	writeJSON(w, http.StatusOK, result)
}

// handleDiskRecommend returns a topology recommendation.
func (s *Server) handleDiskRecommend(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	var req struct {
		Disks []string `json:"disks"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	// Get full disk info
	disks, err := s.talos.GetDiscoveredVolumes()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	// Restrict the recommendation to the disks the user actually selected.
	// A previous version ignored req.Disks and recommended over every
	// discovered volume (including loop devices and the install ISO), which
	// produced wrong topologies (e.g. raidz1 for 5 devices when the user
	// picked 2). Unknown names are dropped; an empty selection (or a
	// selection matching nothing) falls back to all non-system disks so a
	// bare POST without a body still yields a useful recommendation.
	selected := make(map[string]struct{}, len(req.Disks))
	for _, d := range req.Disks {
		selected[d] = struct{}{}
	}
	var filtered []*storage.Disk
	if len(selected) > 0 {
		for _, d := range disks {
			if _, ok := selected[d.DeviceName]; ok {
				filtered = append(filtered, d)
			}
		}
	}
	// candidates is either the user's explicit selection or, when nothing
	// was selected, all discovered disks. We then narrow it to real disks
	// so loop devices and the CD-ROM never shape the recommendation.
	candidates := disks
	if len(filtered) > 0 {
		candidates = filtered
	}
	var real []*storage.Disk
	for _, d := range candidates {
		if isRealDisk(d) {
			real = append(real, d)
		}
	}
	if len(real) > 0 {
		candidates = real
	}

	// Build disk info list
	diskInfos := make([]talos.DiskInfo, 0, len(candidates))
	for _, d := range candidates {
		diskInfos = append(diskInfos, talos.DiskInfo{
			DevicePath:   d.DeviceName,
			Size:         d.Size,
			IsSystemDisk: d.SystemDisk,
			Model:        d.Model,
			Serial:       d.Serial,
		})
	}

	advisor := talos.NewVolumeAdvisor()
	rec, err := advisor.Recommend(diskInfos)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	writeJSON(w, http.StatusOK, rec)
}

// handleVolumes handles UserVolumeConfig operations.
func (s *Server) handleVolumes(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		// List volumes
		writeJSON(w, http.StatusOK, map[string]string{"status": "listing volumes"})
	case http.MethodPost:
		// Create volume
		var req struct {
			Name    string `json:"name"`
			FSType  string `json:"fsType"`
			MinSize string `json:"minSize"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		_, err := talos.UserVolumeConfig(req.Name, req.FSType, req.MinSize)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{"status": "volume created"})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
