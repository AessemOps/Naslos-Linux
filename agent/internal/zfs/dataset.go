package zfs

import (
	"fmt"
	"strings"
)

// AllDatasets lists every dataset on the node with its mountpoint, so callers
// can tell which paths are actually ZFS-backed.
func (c *Client) AllDatasets() ([]Dataset, error) {
	out, err := c.hostExec(zfsBin, "list", "-H", "-o", "name,mountpoint", "-t", "filesystem", "-r")
	if err != nil {
		return nil, fmt.Errorf("listing datasets: %w", err)
	}

	datasets := []Dataset{}
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line == "" {
			continue
		}
		fields := strings.Split(line, "\t")
		if len(fields) < 2 {
			continue
		}
		datasets = append(datasets, Dataset{Name: fields[0], Mountpoint: fields[1]})
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

// CreateDataset creates a new dataset.
func (c *Client) CreateDataset(name string, options map[string]string) error {
	args := []string{"create"}
	for k, v := range options {
		args = append(args, "-o", fmt.Sprintf("%s=%s", k, v))
	}
	args = append(args, name)
	out, err := c.hostExec(zfsBin, args...)
	if err != nil {
		return fmt.Errorf("creating dataset: %s: %w", out, err)
	}
	return nil
}

// DestroyDataset destroys a dataset.
func (c *Client) DestroyDataset(name string, recursive bool) error {
	args := []string{"destroy"}
	if recursive {
		args = append(args, "-r")
	}
	args = append(args, name)
	out, err := c.hostExec(zfsBin, args...)
	if err != nil {
		return fmt.Errorf("destroying dataset: %s: %w", out, err)
	}
	return nil
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
