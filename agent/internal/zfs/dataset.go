package zfs

import (
	"fmt"
	"log"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

// AllDatasets lists every dataset on the node with its mountpoint and space, so
// callers can tell which paths are actually ZFS-backed (and how full they are).
// `used` is requested twice: `-p` gives exact bytes (UsedBytes, for comparisons)
// and the plain form gives the human string the UI shows.
func (c *Client) AllDatasets() ([]Dataset, error) {
	out, err := c.hostExec(zfsBin, "list", "-H", "-p", "-o", "name,used,avail,refer,mountpoint,mounted", "-t", "filesystem", "-r")
	if err != nil {
		return nil, fmt.Errorf("listing datasets: %w", err)
	}

	datasets := []Dataset{}
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line == "" {
			continue
		}
		fields := strings.Split(line, "\t")
		if len(fields) < 5 {
			continue
		}
		// With -p the values are raw bytes; keep the numeric form for callers
		// that compare sizes, and format the human form for display.
		usedBytes, _ := strconv.ParseInt(fields[1], 10, 64)
		availBytes, _ := strconv.ParseInt(fields[2], 10, 64)
		referBytes, _ := strconv.ParseInt(fields[3], 10, 64)
		datasets = append(datasets, Dataset{
			Name:       fields[0],
			Used:       humanBytes(usedBytes),
			UsedBytes:  usedBytes,
			Avail:      humanBytes(availBytes),
			Refer:      humanBytes(referBytes),
			Mountpoint: fields[4],
			Mounted:    len(fields) > 5 && fields[5] == "yes",
		})
	}
	return datasets, nil
}

// humanBytes formats a byte count the way `zfs list` does without -p (powers of
// 1024, one decimal when below 10 in the unit), so the API's dataset listing
// keeps showing the familiar "2.1G" style while the struct also carries exact
// bytes for comparisons.
func humanBytes(n int64) string {
	const unit = 1024
	if n < unit {
		return fmt.Sprintf("%dB", n)
	}
	units := []string{"K", "M", "G", "T", "P", "E"}
	value := float64(n)
	i := -1
	for value >= unit && i < len(units)-1 {
		value /= unit
		i++
	}
	if value < 10 {
		return fmt.Sprintf("%.1f%s", value, units[i])
	}
	return fmt.Sprintf("%.0f%s", value, units[i])
}

// Datasets lists datasets in a pool.
func (c *Client) Datasets(pool string) ([]Dataset, error) {
	if err := ValidatePoolName(pool); err != nil {
		return nil, err
	}
	out, err := c.hostExec(zfsBin, "list", "-H", "-o", "name,used,avail,refer,mountpoint,mounted", "-r", pool)
	if err != nil {
		return nil, fmt.Errorf("listing datasets: %w", err)
	}

	var datasets []Dataset
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line == "" {
			continue
		}
		fields := strings.Split(line, "\t")
		if len(fields) < 5 {
			continue
		}
		datasets = append(datasets, Dataset{
			Name:       fields[0],
			Used:       fields[1],
			Avail:      fields[2],
			Refer:      fields[3],
			Mountpoint: fields[4],
			Mounted:    len(fields) > 5 && fields[5] == "yes",
		})
	}
	return datasets, nil
}

// datasetNamePattern is the ZFS-safe dataset charset, applied to each component
// of a relative name ("media", "photos/2026").
var datasetNamePattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]*$`)

// validCompression are the compression algorithms a dataset may be created with.
var validCompression = map[string]bool{
	"on": true, "off": true, "lz4": true, "zstd": true, "zstd-fast": true,
	"gzip": true, "gzip-1": true, "gzip-9": true, "lzjb": true, "zle": true,
}

// sizePattern matches the sizes ZFS accepts for quota/refquota (100G, 1.5T).
var sizePattern = regexp.MustCompile(`^[0-9]+(\.[0-9]+)?[KMGTPE]?$`)

// datasetOptionKeys are the dataset properties the API may set at creation time.
// Anything else is refused: `zfs create -o` accepts arbitrary properties, and a
// wrong one (e.g. mountpoint=/) would put the dataset somewhere unexpected.
var datasetOptionKeys = map[string]bool{
	"compression": true, "quota": true, "recordsize": true,
	"atime": true, "copies": true, "readonly": true,
}

// ValidateDatasetName validates a dataset name relative to its pool: one or more
// components, each ZFS-safe, with no traversal, snapshot or absolute path.
func ValidateDatasetName(name string) error {
	trimmed := strings.TrimSpace(name)
	if trimmed == "" {
		return invalidf("dataset name is required")
	}
	if trimmed != name {
		return invalidf("dataset name must not start or end with whitespace")
	}
	if len(trimmed) > 200 {
		return invalidf("dataset name must be 200 characters or fewer")
	}
	if strings.HasPrefix(trimmed, "/") {
		return invalidf("dataset name must be relative to its pool, not a path")
	}
	if strings.Contains(trimmed, "@") {
		return invalidf("dataset name must not contain '@' (that is a snapshot)")
	}
	for _, part := range strings.Split(trimmed, "/") {
		if !datasetNamePattern.MatchString(part) {
			return invalidf("invalid dataset name %q: each part must start with a letter or digit and contain only letters, digits, '.', '_' or '-'", trimmed)
		}
	}
	return nil
}

// ValidateDatasetOptions checks the properties a dataset is created with.
func ValidateDatasetOptions(options map[string]string) error {
	for key, value := range options {
		value = strings.TrimSpace(value)
		if !datasetOptionKeys[key] {
			return invalidf("unsupported dataset option %q", key)
		}
		switch key {
		case "compression":
			if !validCompression[value] {
				return invalidf("unsupported compression %q", value)
			}
		case "quota":
			if value != "" && value != "none" && !sizePattern.MatchString(value) {
				return invalidf("invalid quota %q: use a size such as 500G, or none to remove it", value)
			}
		case "recordsize":
			if !sizePattern.MatchString(value) {
				return invalidf("invalid recordsize %q", value)
			}
		case "atime", "readonly":
			if value != "on" && value != "off" {
				return invalidf("%s must be on or off", key)
			}
		case "copies":
			if value != "1" && value != "2" && value != "3" {
				return invalidf("copies must be 1, 2 or 3")
			}
		}
	}
	return nil
}

// CreateDataset creates a new dataset.
//
// name is the full dataset path ("tank/media"), and it must be an existing pool
// plus one or more new components. Options are restricted to the safe set above.
func (c *Client) CreateDataset(name string, options map[string]string) error {
	if err := validateDatasetPath(name); err != nil {
		return err
	}
	if err := ValidateDatasetOptions(options); err != nil {
		return err
	}

	// Create the parent first if the dataset is nested: `zfs create a/b/c`
	// fails unless a/b exists, and creating the intermediate levels is what the
	// operator means. -p makes that one call.
	args := []string{"create", "-p"}
	// Sort the keys so the command is deterministic (map order is random), which
	// keeps logs and tests stable.
	keys := make([]string, 0, len(options))
	for k := range options {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		args = append(args, "-o", fmt.Sprintf("%s=%s", k, options[k]))
	}
	args = append(args, name)

	out, err := c.hostExec(zfsBin, args...)
	if err != nil {
		return fmt.Errorf("creating dataset: %s: %w", strings.TrimSpace(out), err)
	}
	return nil
}

// validateDatasetPath validates a full dataset path: an existing-looking pool
// name followed by one or more dataset components.
func validateDatasetPath(name string) error {
	trimmed := strings.TrimSpace(name)
	if trimmed == "" {
		return invalidf("dataset name is required")
	}
	if strings.HasPrefix(trimmed, "/") {
		return invalidf("dataset %q must be relative to its pool, not an absolute path", name)
	}
	parts := strings.Split(trimmed, "/")
	if len(parts) < 2 {
		return invalidf("dataset %q must be <pool>/<name>", name)
	}
	if err := ValidatePoolName(parts[0]); err != nil {
		return err
	}
	return ValidateDatasetName(strings.Join(parts[1:], "/"))
}

// DestroyDataset destroys a dataset. recursive must be set explicitly to destroy
// a dataset that has children or snapshots: without it ZFS refuses, which is the
// safe default for a UI action.
//
// If the destroy fails with "dataset is busy" it retries once after forcing an
// unmount. That state is real and was hit live (AV-8): a dataset that was once
// mounted inside a pod's mount namespace keeps a stale in-kernel mount record
// after that namespace is gone, so `zfs destroy` refuses forever - the mount
// table shows no mount, there are no snapshots, children or shares, and a node
// reboot does not clear it. `zfs unmount -f` releases the record so the destroy
// can proceed. The unmount is only attempted on that specific failure, so a
// dataset that is genuinely in use (or any other error) is still refused.
func (c *Client) DestroyDataset(name string, recursive bool) error {
	if err := validateDatasetPath(name); err != nil {
		return err
	}

	args := []string{"destroy"}
	if recursive {
		args = append(args, "-r")
	}
	args = append(args, name)

	out, err := c.hostExec(zfsBin, args...)
	if err != nil {
		if !isBusyError(out) {
			return fmt.Errorf("destroying dataset: %s: %w", strings.TrimSpace(out), err)
		}
		// The stale-mount recovery ladder, from least to most invasive:
		//
		// 1. `zfs unmount -f` releases a mount the kernel still tracks in a
		//    namespace that has since gone away.
		// 2. If that reports "not currently mounted", the busy state is not a
		//    real mount at all: ZFS records a mountpoint that a dead namespace
		//    still references, so the guard refuses forever. Setting
		//    `mountpoint=none` drops that claim (the path is no longer ZFS's),
		//    after which the destroy succeeds. This is the documented fix for
		//    exactly this signature in the openzfs tracker (zfs issue #10185,
		//    "sudo zfs set mountpoint=none ... && sudo zfs destroy -f -r").
		//
		// Only a busy failure enters this path, so a dataset that is genuinely
		// in use, or any other error, keeps the original refusal.
		log.Printf("dataset %s is busy; forcing an unmount and retrying", name)
		if unmountOut, unmountErr := c.hostExec(zfsBin, "unmount", "-f", name); unmountErr == nil {
			out, err = c.hostExec(zfsBin, args...)
			if err == nil {
				return nil
			}
		} else if !strings.Contains(strings.ToLower(unmountOut), "not currently mounted") {
			return fmt.Errorf("destroying dataset: it is busy (%s) and the forced unmount also failed: %s: %w",
				strings.TrimSpace(out), strings.TrimSpace(unmountOut), unmountErr)
		}
		// The unmount either failed as "not currently mounted" or the destroy
		// after it still refused: clear the recorded mountpoint and retry once
		// more. The property is only changed when the dataset is already being
		// destroyed, so a live dataset never loses its mountpoint this way.
		// `canmount=off` is set alongside it so a reboot cannot re-establish the
		// mount (and the leaked reference) before the operator retries.
		//
		// A residual case the ladder cannot clear: ZFS occasionally keeps a
		// kernel reference with mounted=no, no snapshots, no children and no
		// holds, which survives reboots. The openzfs tracker's answer there is a
		// pool export/import (see zfs issues #10185, #9606), which this agent
		// deliberately does not do - exporting a pool would take every share on
		// the node offline. The error below names the state so an operator can
		// choose that remedy.
		log.Printf("dataset %s is still busy with no live mount; clearing its mountpoint and retrying", name)
		if propOut, propErr := c.hostExec(zfsBin, "set", "mountpoint=none", "canmount=off", name); propErr != nil {
			return fmt.Errorf("destroying dataset: it is busy with no live mount (%s) and clearing the "+
				"mountpoint also failed: %s: %w", strings.TrimSpace(out), strings.TrimSpace(propOut), propErr)
		}
		out, err = c.hostExec(zfsBin, args...)
		if err != nil {
			if isBusyError(out) {
				return fmt.Errorf("destroying dataset: it is still busy with mounted=no, no snapshots, "+
					"children or holds (%s). This is the openzfs leaked-reference case that needs a pool "+
					"export/import (the agent does not export, as that would take the shares offline)", strings.TrimSpace(out))
			}
			return fmt.Errorf("destroying dataset after clearing its mountpoint: %s: %w", strings.TrimSpace(out), err)
		}
	}
	return nil
}

// isBusyError reports whether a `zfs` failure is the stale-mount "dataset is
// busy" case, which a forced unmount can clear. Anything else is a real refusal
// (in use, wrong path, permissions) and must not trigger an unmount.
func isBusyError(out string) bool {
	return strings.Contains(strings.ToLower(out), "dataset is busy") ||
		strings.Contains(strings.ToLower(out), "cannot destroy") && strings.Contains(strings.ToLower(out), "busy")
}

// DatasetExists reports whether a dataset (or a snapshot) is present.
func (c *Client) DatasetExists(name string) bool {
	_, err := c.hostExec(zfsBin, "list", "-H", "-o", "name", name)
	return err == nil
}

// Snapshot creates a snapshot of a dataset. Both halves reach `zfs snapshot`
// argv, so both are validated (NAS-003).
func (c *Client) Snapshot(dataset, snapName string) error {
	if err := validateDatasetPath(dataset); err != nil {
		return err
	}
	if err := ValidateSnapshotName(snapName); err != nil {
		return err
	}
	name := dataset + "@" + snapName
	out, err := c.hostExec(zfsBin, "snapshot", name)
	if err != nil {
		return fmt.Errorf("creating snapshot: %s: %w", out, err)
	}
	return nil
}

// Snapshots lists snapshots for a dataset.
func (c *Client) Snapshots(dataset string) ([]string, error) {
	if err := validateDatasetPath(dataset); err != nil {
		return nil, err
	}
	out, err := c.hostExec(zfsBin, "list", "-H", "-t", "snapshot", "-o", "name", "-r", dataset)
	if err != nil {
		return nil, fmt.Errorf("listing snapshots: %w", err)
	}

	var snaps []string
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line != "" {
			snaps = append(snaps, strings.TrimPrefix(line, dataset+"@"))
		}
	}
	return snaps, nil
}
