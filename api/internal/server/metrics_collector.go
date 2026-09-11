package server

import (
	"log"
	"os"
	"strconv"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/metrics"
)

// metricsInterval returns how often to poll the Talos node for metrics.
// Overridable via METRICS_INTERVAL_SECONDS for local dev; defaults to 15s.
func metricsInterval() time.Duration {
	if v := os.Getenv("METRICS_INTERVAL_SECONDS"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			return time.Duration(n) * time.Second
		}
	}
	return 15 * time.Second
}

// startMetricsCollector periodically collects node metrics from the Talos
// API and publishes them to the metrics manager. It runs until the process
// exits; collection failures are logged and retried on the next tick.
func (s *Server) startMetricsCollector() {
	if s.talos == nil {
		log.Printf("Metrics collector disabled: no Talos client available")
		return
	}

	interval := metricsInterval()

	// Collect immediately so the dashboard is populated on first load
	// instead of showing zeros until the first tick.
	if err := s.collectMetricsOnce(); err != nil {
		log.Printf("Metrics collection failed: %v", err)
	}

	go func() {
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for range ticker.C {
			if err := s.collectMetricsOnce(); err != nil {
				log.Printf("Metrics collection failed: %v", err)
			}
		}
	}()

	log.Printf("Metrics collector started (interval %s)", interval)
}

// collectMetricsOnce gathers one snapshot of node metrics from Talos,
// converts it to the dashboard metrics model, and publishes it.
func (s *Server) collectMetricsOnce() error {
	tm, err := s.talos.GetSystemMetrics()
	if err != nil {
		return err
	}

	s.metrics.Update(metrics.SystemMetrics{
		CPU: metrics.CPUMetrics{
			UsagePercent: tm.CPU.UsagePercent,
			Cores:        tm.CPU.Cores,
			LoadAvg1:     tm.CPU.LoadAvg1,
			LoadAvg5:     tm.CPU.LoadAvg5,
			LoadAvg15:    tm.CPU.LoadAvg15,
		},
		Memory: metrics.MemoryMetrics{
			Total:        tm.Memory.Total,
			Used:         tm.Memory.Used,
			Free:         tm.Memory.Free,
			Available:    tm.Memory.Available,
			UsagePercent: tm.Memory.UsagePercent,
			SwapTotal:    tm.Memory.SwapTotal,
			SwapUsed:     tm.Memory.SwapUsed,
		},
		Disk: metrics.DiskMetrics{
			Total:        tm.Disk.Total,
			Used:         tm.Disk.Used,
			Free:         tm.Disk.Free,
			UsagePercent: tm.Disk.UsagePercent,
		},
		Network: metrics.NetworkMetrics{
			BytesSent:   tm.Network.BytesSent,
			BytesRecv:   tm.Network.BytesRecv,
			PacketsSent: tm.Network.PacketsSent,
			PacketsRecv: tm.Network.PacketsRecv,
			Interfaces:  convertNetworkInterfaces(tm.Network.Interfaces),
		},
		System: metrics.SystemInfo{
			Hostname:     tm.System.Hostname,
			Uptime:       tm.System.Uptime,
			OS:           tm.System.OS,
			Kernel:       tm.System.Kernel,
			TalosVersion: tm.System.TalosVersion,
		},
	})
	return nil
}
