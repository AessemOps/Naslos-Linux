package zfs

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

// IsZFSAvailable checks if ZFS is usable on the host: the zpool binary
// must exist inside the host root AND the zfs module must be loaded
// (/dev/zfs present). The binary can be present without a loaded module
// (e.g. extension installed but machine.kernel.modules missing), which
// would fail at `zpool create` with a confusing error — this catches it.
func IsZFSAvailable() bool {
	if _, err := os.Stat(hostRoot + zpoolBin); err != nil {
		return false
	}
	if _, err := os.Stat(hostRoot + "/dev/zfs"); err != nil {
		return false
	}
	return true
}

// WaitForDevice waits for a device to appear (after wipefs).
func WaitForDevice(device string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		_, err := os.Stat(device)
		if err == nil {
			return nil
		}
		time.Sleep(500 * time.Millisecond)
	}
	return fmt.Errorf("timeout waiting for device %s", device)
}

// ParseSize parses a human-readable size string to bytes.
func ParseSize(s string) (uint64, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return 0, nil
	}
	var value float64
	var unit string
	if _, err := fmt.Sscanf(s, "%f%s", &value, &unit); err != nil {
		return 0, fmt.Errorf("invalid size %q: %w", s, err)
	}
	switch strings.ToUpper(unit) {
	case "B", "":
		return uint64(value), nil
	case "K", "KB", "KIB":
		return uint64(value * 1024), nil
	case "M", "MB", "MIB":
		return uint64(value * 1024 * 1024), nil
	case "G", "GB", "GIB":
		return uint64(value * 1024 * 1024 * 1024), nil
	case "T", "TB", "TIB":
		return uint64(value * 1024 * 1024 * 1024 * 1024), nil
	}
	return 0, fmt.Errorf("unknown unit %q", unit)
}

// FormatSize formats bytes to human-readable size.
func FormatSize(bytes uint64) string {
	switch {
	case bytes >= 1024*1024*1024*1024:
		return fmt.Sprintf("%.2f TiB", float64(bytes)/(1024*1024*1024*1024))
	case bytes >= 1024*1024*1024:
		return fmt.Sprintf("%.2f GiB", float64(bytes)/(1024*1024*1024))
	case bytes >= 1024*1024:
		return fmt.Sprintf("%.2f MiB", float64(bytes)/(1024*1024))
	case bytes >= 1024:
		return fmt.Sprintf("%.2f KiB", float64(bytes)/1024)
	}
	return strconv.FormatUint(bytes, 10) + " B"
}
