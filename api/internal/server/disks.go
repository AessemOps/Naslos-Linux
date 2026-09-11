package server

import (
	"encoding/json"
	"net/http"

	"github.com/AessemOps/Naslos-Linux/api/internal/talos"
	"github.com/siderolabs/talos/pkg/machinery/api/storage"
)

// handleDisks returns discovered disks.
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

	// Convert to our DiskInfo format
	result := make([]map[string]interface{}, len(disks))
	for i, d := range disks {
		result[i] = map[string]interface{}{
			"device":        d.DeviceName,
			"size":          d.Size,
			"isSystemDisk":  d.SystemDisk,
			"model":         d.Model,
			"serial":        d.Serial,
			"busPath":       d.BusPath,
			"type":          d.Type.String(),
		}
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
	candidates := disks
	if len(filtered) > 0 {
		candidates = filtered
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
