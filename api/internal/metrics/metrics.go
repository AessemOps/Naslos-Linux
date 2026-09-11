// Package metrics provides system monitoring metrics for the Naslos dashboard.
package metrics

import (
	"sync"
	"time"
)

// SystemMetrics holds all system metrics for the dashboard.
type SystemMetrics struct {
	CPU        CPUMetrics        `json:"cpu"`
	Memory     MemoryMetrics     `json:"memory"`
	Disk       DiskMetrics       `json:"disk"`
	Network    NetworkMetrics    `json:"network"`
	ZFS        ZFSMetrics        `json:"zfs"`
	System     SystemInfo        `json:"system"`
	UpdatedAt  time.Time         `json:"updatedAt"`
}

// CPUMetrics holds CPU usage metrics.
type CPUMetrics struct {
	UsagePercent float64 `json:"usagePercent"`
	Cores        int     `json:"cores"`
	LoadAvg1     float64 `json:"loadAvg1"`
	LoadAvg5     float64 `json:"loadAvg5"`
	LoadAvg15    float64 `json:"loadAvg15"`
}

// MemoryMetrics holds memory usage metrics.
type MemoryMetrics struct {
	Total       uint64  `json:"total"`
	Used        uint64  `json:"used"`
	Free        uint64  `json:"free"`
	Available   uint64  `json:"available"`
	UsagePercent float64 `json:"usagePercent"`
	SwapTotal   uint64  `json:"swapTotal"`
	SwapUsed    uint64  `json:"swapUsed"`
}

// DiskMetrics holds disk usage metrics.
type DiskMetrics struct {
	Total       uint64  `json:"total"`
	Used        uint64  `json:"used"`
	Free        uint64  `json:"free"`
	UsagePercent float64 `json:"usagePercent"`
}

// NetworkMetrics holds network interface metrics.
type NetworkMetrics struct {
	BytesSent     uint64 `json:"bytesSent"`
	BytesRecv     uint64 `json:"bytesRecv"`
	PacketsSent   uint64 `json:"packetsSent"`
	PacketsRecv   uint64 `json:"packetsRecv"`
	Interfaces    []NetworkInterface `json:"interfaces"`
}

// NetworkInterface holds per-interface network stats.
type NetworkInterface struct {
	Name       string `json:"name"`
	IPAddress  string `json:"ipAddress"`
	MacAddress string `json:"macAddress"`
	BytesSent  uint64 `json:"bytesSent"`
	BytesRecv  uint64 `json:"bytesRecv"`
}

// ZFSMetrics holds ZFS pool metrics.
type ZFSMetrics struct {
	Pools []PoolMetrics `json:"pools"`
}

// PoolMetrics holds per-pool ZFS metrics.
type PoolMetrics struct {
	Name       string `json:"name"`
	Size       uint64 `json:"size"`
	Alloc      uint64 `json:"alloc"`
	Free       uint64 `json:"free"`
	UsagePercent float64 `json:"usagePercent"`
	Health     string `json:"health"`
}

// SystemInfo holds general system information.
type SystemInfo struct {
	Hostname   string    `json:"hostname"`
	Uptime     uint64    `json:"uptime"`
	OS         string    `json:"os"`
	Kernel     string    `kernel"`
	TalosVersion string  `json:"talosVersion"`
	LastBoot   time.Time `json:"lastBoot"`
}

// Manager manages metrics collection.
type Manager struct {
	mu      sync.Mutex
	metrics SystemMetrics
}

// NewManager creates a new metrics manager.
func NewManager() *Manager {
	return &Manager{}
}

// Get returns the current system metrics.
func (m *Manager) Get() SystemMetrics {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.metrics
}

// Update updates the system metrics.
func (m *Manager) Update(metrics SystemMetrics) {
	m.mu.Lock()
	defer m.mu.Unlock()
	metrics.UpdatedAt = time.Now()
	m.metrics = metrics
}

// GetDashboardData returns data formatted for the dashboard home screen.
func (m *Manager) GetDashboardData() map[string]interface{} {
	m.mu.Lock()
	defer m.mu.Unlock()

	return map[string]interface{}{
		"cpu": map[string]interface{}{
			"usage": m.metrics.CPU.UsagePercent,
			"cores": m.metrics.CPU.Cores,
		},
		"memory": map[string]interface{}{
			"usage":     m.metrics.Memory.UsagePercent,
			"total":     m.metrics.Memory.Total,
			"used":      m.metrics.Memory.Used,
			"available": m.metrics.Memory.Available,
		},
		"disk": map[string]interface{}{
			"usage": m.metrics.Disk.UsagePercent,
			"total": m.metrics.Disk.Total,
			"used":  m.metrics.Disk.Used,
			"free":  m.metrics.Disk.Free,
		},
		"zfs": map[string]interface{}{
			"poolCount": len(m.metrics.ZFS.Pools),
			"pools":     m.metrics.ZFS.Pools,
		},
		"system": map[string]interface{}{
			"hostname": m.metrics.System.Hostname,
			"uptime":   m.metrics.System.Uptime,
			"os":       m.metrics.System.OS,
		},
		"updatedAt": m.metrics.UpdatedAt,
	}
}
