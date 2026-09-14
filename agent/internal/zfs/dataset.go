package zfs

import (
	"fmt"
	"regexp"
	"sort"
	"strings"
)

// AllDatasets lists every dataset on the node with its mountpoint and space, so
// callers can tell which paths are actually ZFS-backed (and how full they are).
func (c *Client) AllDatasets() ([]Dataset, error) {
	out, err := c.hostExec(zfsBin, "list", "-H", "-o", "name,used,avail,refer,mountpoint", "-t", "filesystem", "-r")
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
		datasets = append(datasets, Dataset{
			Name:       fields[0],
			Used:       fields[1],
			Avail:      fields[2],
			Refer:      fields[3],
			Mountpoint: fields[4],
		})
	}
	return datasets, nil
}

// Datasets lists datasets in a pool.
func (c *Client) Datasets(pool string) ([]Dataset, error) {
	out, err := c.hostExec(zfsBin, "list", "-H", "-o", "name,used,avail,refer,mountpoint", "-r", pool)
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
		return fmt.Errorf("dataset name is required")
	}
	if trimmed != name {
		return fmt.Errorf("dataset name must not start or end with whitespace")
	}
	if len(trimmed) > 200 {
		return fmt.Errorf("dataset name must be 200 characters or fewer")
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

// ValidateDatasetOptions checks the properties a dataset is created with.
func ValidateDatasetOptions(options map[string]string) error {
	for key, value := range options {
		value = strings.TrimSpace(value)
		if !datasetOptionKeys[key] {
			return fmt.Errorf("unsupported dataset option %q", key)
		}
		switch key {
		case "compression":
			if !validCompression[value] {
				return fmt.Errorf("unsupported compression %q", value)
			}
		case "quota":
			if value != "" && value != "none" && !sizePattern.MatchString(value) {
				return fmt.Errorf("invalid quota %q: use a size such as 500G, or none to remove it", value)
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
		return fmt.Errorf("dataset name is required")
	}
	if strings.HasPrefix(trimmed, "/") {
		return fmt.Errorf("dataset %q must be relative to its pool, not an absolute path", name)
	}
	parts := strings.Split(trimmed, "/")
	if len(parts) < 2 {
		return fmt.Errorf("dataset %q must be <pool>/<name>", name)
	}
	if err := ValidatePoolName(parts[0]); err != nil {
		return err
	}
	return ValidateDatasetName(strings.Join(parts[1:], "/"))
}

// DestroyDataset destroys a dataset. recursive must be set explicitly to destroy
// a dataset that has children or snapshots: without it ZFS refuses, which is the
// safe default for a UI action.
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
		return fmt.Errorf("destroying dataset: %s: %w", strings.TrimSpace(out), err)
	}
	return nil
}

// DatasetExists reports whether a dataset (or a snapshot) is present.
func (c *Client) DatasetExists(name string) bool {
	_, err := c.hostExec(zfsBin, "list", "-H", "-o", "name", name)
	return err == nil
}

// Snapshot creates a snapshot of a dataset.
func (c *Client) Snapshot(dataset, snapName string) error {
	name := dataset + "@" + snapName
	out, err := c.hostExec(zfsBin, "snapshot", name)
	if err != nil {
		return fmt.Errorf("creating snapshot: %s: %w", out, err)
	}
	return nil
}

// Snapshots lists snapshots for a dataset.
func (c *Client) Snapshots(dataset string) ([]string, error) {
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
