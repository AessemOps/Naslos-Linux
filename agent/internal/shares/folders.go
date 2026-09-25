package shares

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// DatasetsBase is the in-host mount root of the ZFS datasets shares are served
// from. Every folder operation the API can request is confined to it, so the
// folder endpoints cannot be used to read or create anything else on the node.
const DatasetsBase = "/var/mnt"

// zfsSnapshotsDir is ZFS's magic directory for browsing snapshots. It is not
// user data, so it is hidden from the folder picker and refused as a name.
const zfsSnapshotsDir = ".zfs"

// ListFolders returns the names of the immediate subdirectories of an in-host
// directory (sorted). Only directories are returned: the picker is used to point
// a share at a folder, so files would only be noise.
func (c *Client) ListFolders(dirPath string) ([]string, error) {
	clean, err := CleanFolderPath(dirPath)
	if err != nil {
		return nil, err
	}

	entries, err := os.ReadDir(hostPath(clean))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("folder does not exist: %s", clean)
		}
		return nil, fmt.Errorf("reading %s: %w", clean, err)
	}

	folders := make([]string, 0, len(entries))
	for _, entry := range entries {
		// DirEntry.IsDir reports the entry's own type, so a symlink to a
		// directory elsewhere is not followed and not offered.
		if !entry.IsDir() || entry.Name() == zfsSnapshotsDir {
			continue
		}
		folders = append(folders, entry.Name())
	}
	sort.Strings(folders)
	return folders, nil
}

// CreateFolder creates one new folder inside parent and returns its in-host
// path. Only the folder itself is created - the parent must already exist, so a
// mistyped path cannot silently create a chain of directories that then get
// shared.
func (c *Client) CreateFolder(parent, name string) (string, error) {
	cleanParent, err := CleanFolderPath(parent)
	if err != nil {
		return "", err
	}
	if err := ValidateFolderName(name); err != nil {
		return "", err
	}

	info, err := os.Stat(hostPath(cleanParent))
	if err != nil {
		if os.IsNotExist(err) {
			return "", fmt.Errorf("folder does not exist: %s", cleanParent)
		}
		return "", fmt.Errorf("stat %s: %w", cleanParent, err)
	}
	if !info.IsDir() {
		return "", fmt.Errorf("%s is not a folder", cleanParent)
	}

	full := filepath.Join(cleanParent, name)
	if err := os.Mkdir(hostPath(full), 0755); err != nil {
		if os.IsExist(err) {
			return "", fmt.Errorf("folder already exists: %s", full)
		}
		return "", fmt.Errorf("creating %s: %w", full, err)
	}

	return full, nil
}

// DeleteFolder removes an EMPTY directory. It deliberately refuses a non-empty
// one: deleting a share's data is not something a folder picker should be able
// to do, and the host has no recycle bin.
func (c *Client) DeleteFolder(dirPath string) error {
	clean, err := CleanFolderPath(dirPath)
	if err != nil {
		return err
	}
	if clean == DatasetsBase {
		return fmt.Errorf("the datasets root cannot be removed")
	}

	target := hostPath(clean)
	entries, err := os.ReadDir(target)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("folder does not exist: %s", clean)
		}
		return fmt.Errorf("reading %s: %w", clean, err)
	}
	if len(entries) > 0 {
		return fmt.Errorf("folder %s is not empty", clean)
	}

	if err := os.Remove(target); err != nil {
		return fmt.Errorf("removing %s: %w", clean, err)
	}
	return nil
}

// CleanFolderPath canonicalises an in-host folder path and requires it to be the
// datasets base itself or a directory inside it.
//
// filepath.Clean resolves ".." before the prefix check, so "/var/mnt/test/../.."
// is rejected. A string prefix alone is not enough, though: a symlink inside the
// base can point outside it, and the kernel follows symlinks on the actual
// open/mkdir. So an existing path is additionally resolved on the host side and
// re-checked against the resolved base (PF-L5). The base itself is resolved too,
// or a symlinked /var/mnt would be compared against the wrong root.
func CleanFolderPath(path string) (string, error) {
	trimmed := strings.TrimSpace(path)
	if trimmed == "" {
		return "", fmt.Errorf("folder path is required")
	}

	clean := filepath.Clean(trimmed)
	if !strings.HasPrefix(clean, "/") {
		return "", fmt.Errorf("folder path must be absolute: %s", trimmed)
	}
	if clean != DatasetsBase && !strings.HasPrefix(clean, DatasetsBase+string(filepath.Separator)) {
		return "", fmt.Errorf("folder path must be inside %s", DatasetsBase)
	}

	// Resolve symlinks on the host side and re-check containment. A path that
	// does not exist yet (CreateFolder's parent must exist, so this is a genuine
	// miss) stays string-checked; the caller reports "does not exist".
	resolvedBase, err := filepath.EvalSymlinks(hostPath(DatasetsBase))
	if err != nil {
		return clean, nil
	}
	resolvedTarget, err := filepath.EvalSymlinks(hostPath(clean))
	if err != nil {
		if os.IsNotExist(err) {
			return clean, nil
		}
		return "", fmt.Errorf("resolving %s: %w", clean, err)
	}
	rel, err := filepath.Rel(resolvedBase, resolvedTarget)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("folder path must resolve inside %s", DatasetsBase)
	}

	return clean, nil
}

// ValidateFolderName rejects names that are empty, are a path traversal, contain
// separators or control characters, or collide with ZFS's reserved directory.
func ValidateFolderName(name string) error {
	if strings.TrimSpace(name) == "" {
		return fmt.Errorf("folder name is required")
	}
	if strings.TrimSpace(name) != name {
		return fmt.Errorf("folder name must not start or end with whitespace")
	}
	if len(name) > 255 {
		return fmt.Errorf("folder name must be 255 characters or fewer")
	}
	if name == "." || name == ".." {
		return fmt.Errorf("folder name must not be %q", name)
	}
	if strings.ContainsAny(name, "/\\") {
		return fmt.Errorf("folder name must not contain a path separator")
	}
	if strings.ContainsAny(name, "\n\r\t\x00") {
		return fmt.Errorf("folder name must not contain control characters")
	}
	if name == zfsSnapshotsDir {
		return fmt.Errorf("%s is reserved by ZFS for snapshots", zfsSnapshotsDir)
	}
	return nil
}
