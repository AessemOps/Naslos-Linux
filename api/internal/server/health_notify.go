package server

import (
	"fmt"
	"log"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/notifications"
)

// healthNotifyInterval is how often the health watcher re-checks pool health and
// disk presence. Overridable via HEALTH_NOTIFY_INTERVAL_SECONDS; defaults to 5
// minutes, which is far more than enough for hardware health and keeps the agent
// and Talos APIs quiet.
func healthNotifyInterval() time.Duration {
	if v := os.Getenv("HEALTH_NOTIFY_INTERVAL_SECONDS"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			return time.Duration(n) * time.Second
		}
	}
	return 5 * time.Minute
}

// healthWatcher remembers the last observed health of each pool and the set of
// disks that were present, so a notification fires on a *transition* rather than
// on every poll (PF-M9).
type healthWatcher struct {
	mu        sync.Mutex
	primed    bool
	disksSeen bool
	pools     map[string]string
	disks     map[string]struct{}
}

// startHealthNotifier drives the two wired notification events: zfs_health and
// disk_failure. It is a no-op at each tick when notifications are disabled or the
// event is not subscribed, so it is always safe to run.
func (s *Server) startHealthNotifier() {
	if s.agent == nil && s.talos == nil {
		return
	}
	interval := healthNotifyInterval()
	w := &healthWatcher{pools: map[string]string{}, disks: map[string]struct{}{}}
	go func() {
		w.check(s)
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for range ticker.C {
			w.check(s)
		}
	}()
	log.Printf("Health notifier started (interval %s)", interval)
}

// check runs one poll of pool health and disk presence.
func (w *healthWatcher) check(s *Server) {
	mgr := s.notificationManager()
	if mgr == nil || !mgr.GetSettings().Enabled {
		return
	}
	w.checkPools(s, mgr)
	w.checkDisks(s, mgr)
}

// checkPools alerts when a pool leaves ONLINE, and once when it returns.
func (w *healthWatcher) checkPools(s *Server, mgr *notifications.Manager) {
	if s.agent == nil {
		return
	}
	pools, err := s.agent.ListPools()
	if err != nil {
		log.Printf("health notifier: listing pools: %v", err)
		return
	}

	w.mu.Lock()
	defer w.mu.Unlock()
	first := !w.primed
	for _, pool := range pools {
		last, seen := w.pools[pool.Name]
		w.pools[pool.Name] = pool.Health
		if first || !seen || last == pool.Health {
			continue
		}
		if !mgr.EventEnabled(notifications.EventZFSHealth) {
			continue
		}
		severity := notifications.SeverityError
		tags := []string{"warning", "naslos", "zfs"}
		verb := "is no longer healthy"
		if strings.EqualFold(pool.Health, "ONLINE") {
			severity = notifications.SeverityInfo
			tags = []string{"white_check_mark", "naslos", "zfs"}
			verb = "is healthy again"
		}
		_ = mgr.Send(notifications.Notification{
			Title:    fmt.Sprintf("ZFS pool %s %s", pool.Name, verb),
			Message:  fmt.Sprintf("pool %s health: %s -> %s", pool.Name, last, pool.Health),
			Severity: severity,
			Tags:     tags,
			Time:     time.Now(),
		})
	}
	w.primed = true
}

// checkDisks alerts when a disk that was present disappears. The first poll only
// records the baseline, so a daemon restart does not report every disk as lost.
func (w *healthWatcher) checkDisks(s *Server, mgr *notifications.Manager) {
	if s.talos == nil {
		return
	}
	disks, err := s.talos.GetDiscoveredVolumes()
	if err != nil {
		log.Printf("health notifier: listing disks: %v", err)
		return
	}
	current := make(map[string]string, len(disks)) // key -> human label
	for _, disk := range disks {
		key := disk.Serial
		if key == "" {
			key = disk.DeviceName
		}
		label := disk.DeviceName
		if disk.Model != "" {
			label = fmt.Sprintf("%s (%s)", disk.DeviceName, disk.Model)
		}
		current[key] = label
	}

	w.mu.Lock()
	defer w.mu.Unlock()
	if w.pools == nil {
		w.pools = map[string]string{}
	}
	if w.disks == nil {
		w.disks = map[string]struct{}{}
	}
	if !w.disksSeen {
		for key := range current {
			w.disks[key] = struct{}{}
		}
		w.disksSeen = true
		return
	}

	missing := make([]string, 0)
	for key := range w.disks {
		if _, ok := current[key]; !ok {
			missing = append(missing, key)
		}
	}
	sort.Strings(missing)
	if len(missing) > 0 && mgr.EventEnabled(notifications.EventDiskFailure) {
		_ = mgr.Send(notifications.Notification{
			Title:    "Disk no longer visible",
			Message:  fmt.Sprintf("%d disk(s) disappeared from the node: %s", len(missing), strings.Join(missing, ", ")),
			Severity: notifications.SeverityCritical,
			Tags:     []string{"rotating_light", "naslos", "disk"},
			Time:     time.Now(),
		})
	}
	w.disks = make(map[string]struct{}, len(current))
	for key := range current {
		w.disks[key] = struct{}{}
	}
}
