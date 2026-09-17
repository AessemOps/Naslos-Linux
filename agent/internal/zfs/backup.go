package zfs

import (
	"context"
	"fmt"
	"io"
	"os/exec"
	"regexp"
	"strconv"
	"strings"
)

// Backup and restore need something the other operations do not: a *stream*. A
// `zfs send` stream can be terabytes long, so the agent hands it out as it is
// produced instead of buffering it - the reason the agent could not be a backup
// sender before.
//
// These are the only commands here that move bulk data, so both ends are seams
// (`streamHostOut`/`streamHostIn`) that tests can replace: the arguments decide
// what a backup contains and what a restore overwrites.

// snapshotNamePattern is what a snapshot component may look like.
var snapshotNamePattern = regexp.MustCompile(`^[A-Za-z0-9_.:+-]+$`)

// ValidateSnapshotName rejects anything that could turn a snapshot argument into
// a flag or a second argument. Snapshot names are the one piece of backup input
// that reaches a command line.
func ValidateSnapshotName(name string) error {
	if strings.TrimSpace(name) == "" {
		return fmt.Errorf("snapshot name is required")
	}
	if strings.HasPrefix(name, "-") {
		return fmt.Errorf("snapshot name %q must not start with '-'", name)
	}
	if strings.ContainsAny(name, "@ /\\") {
		return fmt.Errorf("snapshot name %q must not contain '@', '/', '\\' or spaces", name)
	}
	if !snapshotNamePattern.MatchString(name) {
		return fmt.Errorf("snapshot name %q must be letters, digits, '.', '_', ':', '+' or '-'", name)
	}
	return nil
}

// SendStreamOptions describes one `zfs send`.
type SendStreamOptions struct {
	// Dataset is the dataset to send (`pool/name`).
	Dataset string
	// To is the snapshot to send, without the `dataset@` prefix.
	To string
	// From is the optional base snapshot: set it for an incremental send.
	From string
	// Raw sends the encrypted records as they are (-w), which is what a backup
	// wants: the receiver never needs to decrypt, and the key stays with ZFS.
	Raw bool
}

// SendCommandLine builds the argument list for `zfs send`. It is separated from
// execution so tests can pin it: this is the single place that decides what a
// backup contains.
func SendCommandLine(opts SendStreamOptions) ([]string, error) {
	if err := validateDatasetPath(opts.Dataset); err != nil {
		return nil, err
	}
	if err := ValidateSnapshotName(opts.To); err != nil {
		return nil, fmt.Errorf("to: %w", err)
	}

	args := []string{"send"}
	if opts.Raw {
		args = append(args, "-w")
	}
	if opts.From != "" {
		if err := ValidateSnapshotName(opts.From); err != nil {
			return nil, fmt.Errorf("from: %w", err)
		}
		if opts.From == opts.To {
			return nil, fmt.Errorf("base and target are the same snapshot (%s): an incremental send needs two", opts.To)
		}
		args = append(args, "-i", opts.Dataset+"@"+opts.From)
	}
	return append(args, opts.Dataset+"@"+opts.To), nil
}

// SendStream starts a `zfs send` and returns its stdout plus a waiter. The waiter
// must be called after the stream is drained: it reports the command's failure,
// which is the only way to learn that a send died in the middle.
func (c *Client) SendStream(opts SendStreamOptions) (io.ReadCloser, func() error, error) {
	args, err := SendCommandLine(opts)
	if err != nil {
		return nil, nil, err
	}
	return streamHostOut(c.ctx, zfsBin, args...)
}

// EstimateSend asks ZFS how large the stream would be (a dry run), so a sender can
// show progress and - more importantly - tell a truncated stream from a complete
// one by comparing what it pushed against this number.
func (c *Client) EstimateSend(opts SendStreamOptions) (int64, error) {
	args, err := SendCommandLine(opts)
	if err != nil {
		return 0, err
	}
	// -n = dry run, -P = parsable ("size\t<bytes>").
	withDryRun := append([]string{"send", "-n", "-P"}, args[1:]...)

	out, err := c.hostExec(zfsBin, withDryRun...)
	if err != nil {
		return 0, fmt.Errorf("estimating send size: %s: %w", strings.TrimSpace(out), err)
	}
	return parseSendSize(out)
}

// parseSendSize reads the `size\t<bytes>` line of a parsable dry run.
func parseSendSize(out string) (int64, error) {
	for _, line := range strings.Split(out, "\n") {
		fields := strings.SplitN(strings.TrimSpace(line), "\t", 2)
		if len(fields) != 2 || fields[0] != "size" {
			continue
		}
		size, err := strconv.ParseInt(strings.TrimSpace(fields[1]), 10, 64)
		if err != nil {
			return 0, fmt.Errorf("unexpected size %q from zfs send -nP: %w", fields[1], err)
		}
		return size, nil
	}
	return 0, fmt.Errorf("zfs send -nP did not report a size")
}

// SnapshotInfo is a snapshot's identity: its name plus the GUID that ties it to the
// stream a peer stored. GUIDs are how a sender recognises "the snapshot I sent last
// time" even if it was renamed, which is what makes incrementals safe rather than
// best-effort.
type SnapshotInfo struct {
	Name    string `json:"name"`
	GUID    string `json:"guid"`
	Created string `json:"created"`
}

// SnapshotsWithGUID lists a dataset's snapshots with their GUIDs, in creation
// order.
func (c *Client) SnapshotsWithGUID(dataset string) ([]SnapshotInfo, error) {
	if err := validateDatasetPath(dataset); err != nil {
		return nil, err
	}
	out, err := c.hostExec(zfsBin, "list", "-H", "-p", "-t", "snapshot",
		"-o", "name,guid,creation", "-s", "creation", "-r", dataset)
	if err != nil {
		return nil, fmt.Errorf("listing snapshots: %w", err)
	}

	snapshots := []SnapshotInfo{}
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line == "" {
			continue
		}
		fields := strings.Split(line, "\t")
		if len(fields) < 2 {
			continue
		}
		name := fields[0]
		if at := strings.Index(name, "@"); at >= 0 {
			name = name[at+1:]
		}
		info := SnapshotInfo{Name: name, GUID: fields[1]}
		if len(fields) > 2 {
			info.Created = fields[2]
		}
		snapshots = append(snapshots, info)
	}
	return snapshots, nil
}

// ReceiveStream starts `zfs receive` and returns its stdin plus a waiter. The
// caller streams a `zfs send` stream into it; ZFS itself refuses a truncated
// stream, which is the strongest evidence available that a restore is complete.
func (c *Client) ReceiveStream(dataset string, force bool) (io.WriteCloser, func() error, error) {
	if err := validateDatasetPath(dataset); err != nil {
		return nil, nil, err
	}
	args := []string{"receive"}
	if force {
		args = append(args, "-F")
	}
	args = append(args, dataset)
	return streamHostIn(c.ctx, zfsBin, args...)
}

// streamHostOut runs a host command and hands back its stdout as it is produced.
var streamHostOut = func(ctx context.Context, name string, args ...string) (io.ReadCloser, func() error, error) {
	cmd, stderr := hostCommand(ctx, name, args...)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, nil, err
	}
	return stdout, hostWait(cmd, stderr), nil
}

// streamHostIn runs a host command and hands back its stdin.
var streamHostIn = func(ctx context.Context, name string, args ...string) (io.WriteCloser, func() error, error) {
	cmd, stderr := hostCommand(ctx, name, args...)
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, nil, err
	}
	return stdin, hostWait(cmd, stderr), nil
}

// hostCommand builds a command that runs inside the host's chroot.
func hostCommand(ctx context.Context, name string, args ...string) (*exec.Cmd, *strings.Builder) {
	cmd := exec.CommandContext(ctx, "chroot", hostRoot, name)
	cmd.Args = append(cmd.Args, args...)
	stderr := &strings.Builder{}
	cmd.Stderr = stderr
	return cmd, stderr
}

// hostWait returns a waiter that reports the command's stderr on failure. ZFS
// explains a failed send or receive on stderr, and without it the caller sees only
// "exit status 1".
func hostWait(cmd *exec.Cmd, stderr *strings.Builder) func() error {
	return func() error {
		if err := cmd.Wait(); err != nil {
			msg := strings.TrimSpace(stderr.String())
			if msg == "" {
				msg = err.Error()
			}
			return fmt.Errorf("zfs: %s", msg)
		}
		return nil
	}
}
