// Command buddyctl is the operator client for Buddy Backup: it creates the key
// that identifies an instance to its buddies, enrolls that key on a receiver, and
// pushes or restores backups through the same protocol the UI uses.
//
// Everything it uploads is encrypted on this side of the wire, so the receiver -
// a second Naslos instance or a plain Docker volume - stores ciphertext it cannot
// read. See docs/buddy-backup.md.
package main

import (
	"archive/tar"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

const usage = `buddyctl - Buddy Backup client

Usage: buddyctl <command> [flags]

Commands:
  identity                     create (or show) this instance's key material
  enroll    <receiver-url>     authorize this key on a receiver (needs its token)
  status    <receiver-url>     free space, quota and last backup on the receiver
  backups   <receiver-url>     list the backups stored for this key
  push      <receiver-url>     encrypt and upload a directory, file or stream
  restore   <receiver-url>     download, verify and decrypt a backup
  prune     <receiver-url>     keep only the newest N chains of a source

Global identity file: --identity (default $BUDDY_IDENTITY or ~/.naslos/buddy-identity.json)

Examples:
  buddyctl identity --name naslos-a
  buddyctl enroll https://naslos-b --token one-time-token --sources naslos-a/
  buddyctl push https://naslos-b --source naslos-a/data --dir /var/mnt/tank/data
  buddyctl restore https://naslos-b --source naslos-a/data --dir ./restored
  buddyctl status https://naslos-b
`

func main() {
	if len(os.Args) < 2 {
		fmt.Fprint(os.Stderr, usage)
		os.Exit(2)
	}

	var err error
	switch os.Args[1] {
	case "identity":
		err = cmdIdentity(os.Args[2:])
	case "enroll":
		err = cmdEnroll(os.Args[2:])
	case "status":
		err = cmdStatus(os.Args[2:])
	case "backups":
		err = cmdBackups(os.Args[2:])
	case "push":
		err = cmdPush(os.Args[2:])
	case "restore":
		err = cmdRestore(os.Args[2:])
	case "prune":
		err = cmdPrune(os.Args[2:])
	case "-h", "--help", "help":
		fmt.Print(usage)
		return
	default:
		fmt.Fprintf(os.Stderr, "unknown command %q\n\n%s", os.Args[1], usage)
		os.Exit(2)
	}

	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}

// defaultIdentityPath is where the key material lives unless overridden.
func defaultIdentityPath() string {
	if env := os.Getenv("BUDDY_IDENTITY"); env != "" {
		return env
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "buddy-identity.json"
	}
	return filepath.Join(home, ".naslos", "buddy-identity.json")
}

// loadIdentity reads the key file, creating it on first use so the first command
// an operator runs is the one that produces their identity.
func loadIdentity(path, name string) (*buddy.Identity, error) {
	if path == "" {
		path = defaultIdentityPath()
	}
	if _, err := os.Stat(path); err != nil {
		if !os.IsNotExist(err) {
			return nil, err
		}
		if name == "" {
			host, _ := os.Hostname()
			name = host
		}
		if name == "" {
			name = "naslos"
		}
		identity, err := buddy.NewIdentity(strings.ToLower(name))
		if err != nil {
			return nil, err
		}
		if err := identity.Save(path); err != nil {
			return nil, err
		}
		fmt.Fprintf(os.Stderr, "created identity %s (%s)\n", path, identity.Name)
	}
	return buddy.LoadIdentity(path)
}

// client builds a sender for a receiver URL.
func client(receiverURL string, identity *buddy.Identity) *buddy.Client {
	return buddy.NewClient(receiverURL, identity)
}

// printJSON writes an indented JSON document to stdout.
func printJSON(value any) error {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(value)
}

// humanBytes renders a byte count for humans.
func humanBytes(n int64) string {
	const unit = 1024
	if n < unit {
		return fmt.Sprintf("%d B", n)
	}
	div, exp := int64(unit), 0
	for m := n / unit; m >= unit; m /= unit {
		div *= unit
		exp++
	}
	return fmt.Sprintf("%.1f %ciB", float64(n)/float64(div), "KMGTPE"[exp])
}

// sortedPaths walks a directory and returns its files in a stable order, so the
// same tree always produces the same stream.
func sortedPaths(root string) ([]string, error) {
	var paths []string
	err := filepath.Walk(root, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.Mode().IsRegular() {
			paths = append(paths, path)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(paths)
	return paths, nil
}

// cmdIdentity creates or displays this instance's key material. The public key it
// prints is what a receiver authorizes; the private key and the KEK never leave
// this file.
func cmdIdentity(args []string) error {
	flags := flag.NewFlagSet("identity", flag.ExitOnError)
	identityFlag := flags.String("identity", "", "identity file (default: $BUDDY_IDENTITY or ~/.naslos/buddy-identity.json)")
	nameFlag := flags.String("name", "", "label for a new identity (defaults to the hostname)")
	jsonFlag := flags.Bool("json", false, "print the public key and fingerprint as JSON")
	if err := flags.Parse(args); err != nil {
		return err
	}

	identity, err := loadIdentity(*identityFlag, *nameFlag)
	if err != nil {
		return err
	}
	fingerprint, err := identity.Fingerprint()
	if err != nil {
		return err
	}

	if *jsonFlag {
		return printJSON(map[string]string{
			"name":        identity.Name,
			"publicKey":   identity.PublicKey,
			"fingerprint": fingerprint,
			"identity":    *identityFlag,
		})
	}

	fmt.Printf("name:        %s\n", identity.Name)
	fmt.Printf("fingerprint: %s\n", fingerprint)
	fmt.Printf("public key:  %s\n", identity.PublicKey)
	fmt.Printf("\nAuthorize this key on a receiver with:\n  buddyctl enroll <receiver-url> --token <token>\n")
	fmt.Printf("or add the public key above to the receiver's peer list.\n")
	return nil
}

// cmdEnroll authorizes this key on a receiver that has an enrollment token.
func cmdEnroll(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: buddyctl enroll <receiver-url> --token <token> [--sources a/ b/]")
	}
	receiverURL := args[0]

	flags := flag.NewFlagSet("enroll", flag.ExitOnError)
	identityFlag := flags.String("identity", "", "identity file")
	tokenFlag := flags.String("token", "", "enrollment token shown by the receiver")
	nameFlag := flags.String("name", "", "name to register (defaults to the identity name)")
	sourcesFlag := flags.String("sources", "", "comma-separated source prefixes this key may write (default: any)")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}
	if *tokenFlag == "" {
		return fmt.Errorf("--token is required: it is on the receiver's Buddy Backup page")
	}

	identity, err := loadIdentity(*identityFlag, "")
	if err != nil {
		return err
	}
	name := *nameFlag
	if name == "" {
		name = identity.Name
	}

	var sources []string
	for _, source := range strings.Split(*sourcesFlag, ",") {
		if source = strings.TrimSpace(source); source != "" {
			sources = append(sources, source)
		}
	}

	fingerprint, err := client(receiverURL, identity).Enroll(*tokenFlag, name, sources)
	if err != nil {
		return err
	}
	fmt.Printf("enrolled as %s (%s)\n", name, fingerprint)
	return nil
}

// cmdStatus reports what the receiver knows: space, quota and last backup.
func cmdStatus(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: buddyctl status <receiver-url>")
	}
	flags := flag.NewFlagSet("status", flag.ExitOnError)
	identityFlag := flags.String("identity", "", "identity file")
	jsonFlag := flags.Bool("json", false, "print the raw status document")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}

	identity, err := loadIdentity(*identityFlag, "")
	if err != nil {
		return err
	}
	status, err := client(args[0], identity).Status()
	if err != nil {
		return err
	}
	if *jsonFlag {
		return printJSON(status)
	}

	fmt.Printf("receiver:      %s (protocol v%d, %s)\n", status.Receiver, status.ProtocolVer, status.Version)
	fmt.Printf("free space:    %s\n", humanBytes(status.FreeBytes))
	fmt.Printf("stored for me: %s", humanBytes(status.UsedBytes))
	if status.QuotaBytes > 0 {
		fmt.Printf(" of %s", humanBytes(status.QuotaBytes))
	}
	fmt.Println()
	if status.LastBackup != nil {
		fmt.Printf("last backup:   %s (%s ago)\n", status.LastBackup.Format("2006-01-02 15:04:05"), time.Since(*status.LastBackup).Truncate(time.Second))
	} else {
		fmt.Printf("last backup:   never\n")
	}
	fmt.Printf("sources:       %s\n", strings.Join(status.Sources, ", "))
	fmt.Printf("peers allowed: %s\n", strings.Join(status.PeerNames, ", "))
	if status.EnrollOpen {
		fmt.Printf("enrollment:    open (a token is configured)\n")
	}
	return nil
}

// cmdPush encrypts a directory, file or stream and uploads it. An interrupted
// push leaves a state file behind: re-running with --resume continues the same
// chain instead of re-uploading what already arrived.
func cmdPush(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: buddyctl push <receiver-url> --source <name> [--dir DIR | --file FILE | <stream]")
	}
	receiverURL := args[0]

	flags := flag.NewFlagSet("push", flag.ExitOnError)
	identityFlag := flags.String("identity", "", "identity file")
	sourceFlag := flags.String("source", "", "logical name of this backup on the receiver (e.g. naslos-a/data)")
	dirFlag := flags.String("dir", "", "directory to archive and send as tar")
	fileFlag := flags.String("file", "", "single file to send")
	kindFlag := flags.String("kind", "", "payload label: tar, zfs-send, raw (default: tar for --dir, raw otherwise)")
	stateFlag := flags.String("state", "", "resume-state file (default: ~/.naslos/buddy-state-<source>.json)")
	resumeFlag := flags.Bool("resume", false, "continue the interrupted push recorded in the state file")
	pruneKeep := flags.Int("prune-keep", 0, "after a successful push, keep only the newest N chains on the receiver")
	quietFlag := flags.Bool("quiet", false, "suppress progress output")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}
	if *sourceFlag == "" {
		return fmt.Errorf("--source is required: it is the name this backup is stored under")
	}
	if err := buddy.ValidateSource(*sourceFlag); err != nil {
		return err
	}

	var (
		reader io.Reader
		closer func()
		kind   = *kindFlag
	)
	switch {
	case *dirFlag != "":
		paths, err := sortedPaths(*dirFlag)
		if err != nil {
			return err
		}
		reader, closer = tarStream(*dirFlag, paths)
		if kind == "" {
			kind = "tar"
		}
	case *fileFlag != "":
		file, err := os.Open(*fileFlag)
		if err != nil {
			return err
		}
		reader, closer = file, func() { file.Close() }
		if kind == "" {
			kind = "raw"
		}
	default:
		// A stream: `zfs send -w ... | buddyctl push URL --source X --kind zfs-send`
		reader, closer = os.Stdin, func() {}
		if kind == "" {
			kind = "raw"
		}
	}
	defer closer()

	identity, err := loadIdentity(*identityFlag, "")
	if err != nil {
		return err
	}

	statePath := *stateFlag
	if statePath == "" {
		statePath = defaultStatePath(*sourceFlag)
	}
	var state *buddy.ChainState
	if *resumeFlag {
		state, err = buddy.LoadChainState(statePath)
		if err != nil {
			return fmt.Errorf("cannot resume: %w", err)
		}
	}
	if state == nil {
		state, err = buddy.NewChainState(*sourceFlag, kind)
		if err != nil {
			return err
		}
	}

	// The state carries the chain's data key, so it is written (0600) *before*
	// the first chunk goes out: a crash mid-push must not lose the ability to
	// finish, and it must not be readable by anyone else meanwhile.
	if err := state.Save(statePath); err != nil {
		return err
	}

	var progress func(buddy.PushProgress)
	if !*quietFlag {
		progress = func(p buddy.PushProgress) {
			if p.Chunk%32 == 0 || p.Chunk == 0 {
				fmt.Fprintf(os.Stderr, "\r  %s sent, %s stored (%d chunks, %d already there)",
					humanBytes(p.PlainBytes), humanBytes(p.SealedBytes), p.Chunk+1, p.Skipped)
			}
		}
	}

	result, err := client(receiverURL, identity).Push(buddy.PushOptions{
		Source:    *sourceFlag,
		Kind:      kind,
		Reader:    reader,
		State:     state,
		PruneKeep: *pruneKeep,
		Progress:  progress,
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "\nthe push did not finish. Re-run with --resume to continue chain %s\n", state.Chain)
		return err
	}

	// Published: the chain is complete, so the resume state is no longer needed.
	_ = os.Remove(statePath)

	if !*quietFlag {
		fmt.Fprintln(os.Stderr)
	}
	fmt.Printf("backed up %s to chain %s: %s in %d chunks (%s stored, %d uploaded, %d already present)\n",
		result.Source, result.Chain, humanBytes(result.PlainBytes), result.Chunks,
		humanBytes(result.SealedBytes), result.Uploaded, result.Skipped)
	if result.PrunedChains > 0 {
		fmt.Printf("pruned %d older chains\n", result.PrunedChains)
	}
	return nil
}

// cmdBackups lists what this key has stored on a receiver.
func cmdBackups(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: buddyctl backups <receiver-url>")
	}
	flags := flag.NewFlagSet("backups", flag.ExitOnError)
	identityFlag := flags.String("identity", "", "identity file")
	jsonFlag := flags.Bool("json", false, "print the raw backup list")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}

	identity, err := loadIdentity(*identityFlag, "")
	if err != nil {
		return err
	}
	backups, err := client(args[0], identity).Backups()
	if err != nil {
		return err
	}
	if *jsonFlag {
		return printJSON(backups)
	}
	if len(backups) == 0 {
		fmt.Println("no backups stored for this key")
		return nil
	}

	fmt.Printf("%-28s %-6s %-20s %s\n", "SOURCE", "KIND", "LAST BACKUP", "STORED")
	for _, backup := range backups {
		fmt.Printf("%-28s %-6s %-20s %s (%d chunks)\n",
			backup.Source,
			backup.Kind,
			backup.CreatedAt.Local().Format("2006-01-02 15:04:05"),
			humanBytes(backup.StoredBytes),
			backup.Chunks)
	}
	return nil
}

// cmdRestore downloads, verifies and decrypts a backup. It verifies the manifest
// signature and every chunk before writing anything: a restore that would produce
// silently corrupt data is a restore that fails instead.
func cmdRestore(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: buddyctl restore <receiver-url> --source <name> [--dir DIR | --out FILE | --verify-only]")
	}
	receiverURL := args[0]

	flags := flag.NewFlagSet("restore", flag.ExitOnError)
	identityFlag := flags.String("identity", "", "identity file")
	sourceFlag := flags.String("source", "", "logical name of the backup to restore")
	chainFlag := flags.String("chain", "", "chain to restore (default: the current one)")
	dirFlag := flags.String("dir", "", "extract a tar backup into this directory")
	outFlag := flags.String("out", "", "write the restored stream to this file")
	verifyFlag := flags.Bool("verify-only", false, "decrypt and verify without writing anything")
	quietFlag := flags.Bool("quiet", false, "suppress progress output")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}
	if *sourceFlag == "" {
		return fmt.Errorf("--source is required")
	}
	if err := buddy.ValidateSource(*sourceFlag); err != nil {
		return err
	}

	identity, err := loadIdentity(*identityFlag, "")
	if err != nil {
		return err
	}

	var (
		out    io.Writer
		closer func() error
	)
	switch {
	case *verifyFlag:
		out, closer = io.Discard, func() error { return nil }
	case *dirFlag != "":
		extractor := newTarExtractor(*dirFlag)
		out, closer = extractor, extractor.Close
	case *outFlag != "":
		file, err := os.Create(*outFlag)
		if err != nil {
			return err
		}
		out, closer = file, file.Close
	default:
		// stdout: `buddyctl restore URL --source X | zfs receive pool/restored`
		out, closer = os.Stdout, func() error { return nil }
	}

	var progress func(buddy.RestoreProgress)
	if !*quietFlag {
		progress = func(p buddy.RestoreProgress) {
			fmt.Fprintf(os.Stderr, "\r  %s verified of %d chunks", humanBytes(p.PlainBytes), p.Chunks)
		}
	}

	result, err := client(receiverURL, identity).Restore(buddy.RestoreOptions{
		Source:   *sourceFlag,
		Chain:    *chainFlag,
		Out:      out,
		Progress: progress,
	})
	if err != nil {
		return err
	}
	if err := closer(); err != nil {
		return err
	}
	if !*quietFlag {
		fmt.Fprintln(os.Stderr)
	}
	fmt.Printf("restored %s from chain %s: %s in %d chunks (kind %s, taken %s)\n",
		result.Source, result.Chain, humanBytes(result.PlainBytes), result.Chunks,
		result.Manifest.Kind, result.Manifest.CreatedAt.Local().Format("2006-01-02 15:04:05"))
	if *verifyFlag {
		fmt.Println("the backup is intact: every chunk decrypted and matched the signed manifest")
	}
	return nil
}

// cmdPrune asks a receiver to keep only the newest N chains of a source.
func cmdPrune(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: buddyctl prune <receiver-url> --source <name> --keep N")
	}
	flags := flag.NewFlagSet("prune", flag.ExitOnError)
	identityFlag := flags.String("identity", "", "identity file")
	sourceFlag := flags.String("source", "", "logical name of the backup source")
	keepFlag := flags.Int("keep", 0, "how many chains to keep (at least 1)")
	if err := flags.Parse(args[1:]); err != nil {
		return err
	}
	if *sourceFlag == "" {
		return fmt.Errorf("--source is required")
	}
	if *keepFlag < 1 {
		return fmt.Errorf("--keep must be at least 1")
	}

	identity, err := loadIdentity(*identityFlag, "")
	if err != nil {
		return err
	}
	removed, err := client(args[0], identity).Prune(*sourceFlag, *keepFlag)
	if err != nil {
		return err
	}
	fmt.Printf("kept the newest %d chains of %s, removed %d\n", *keepFlag, *sourceFlag, removed)
	return nil
}

// defaultStatePath is where an interrupted push records how to continue.
func defaultStatePath(source string) string {
	home, err := os.UserHomeDir()
	if err != nil {
		home = "."
	}
	name := strings.NewReplacer("/", "_", "\\", "_").Replace(source)
	return filepath.Join(home, ".naslos", "buddy-state-"+name+".json")
}

// tarStream archives a directory as a tar stream. The archiver runs in a
// goroutine feeding a pipe, so a multi-terabyte tree is never held in memory and
// the encryption can start on the first bytes that arrive.
//
// Only regular files and directories are preserved; symlinks, devices and
// extended attributes are not sent in v1 (documented in docs/buddy-backup.md).
func tarStream(root string, paths []string) (io.Reader, func()) {
	pr, pw := io.Pipe()
	go func() {
		writer := tar.NewWriter(pw)
		err := writeTar(writer, root, paths)
		if closeErr := writer.Close(); err == nil {
			err = closeErr
		}
		// A nil error closes the pipe cleanly, which is how the sender sees EOF.
		_ = pw.CloseWithError(err)
	}()
	return pr, func() { _ = pr.Close() }
}

// writeTar writes the given files (with their parent directories) into a tar
// stream, in the order it is given.
func writeTar(writer *tar.Writer, root string, paths []string) error {
	written := map[string]bool{}
	for _, path := range paths {
		info, err := os.Stat(path)
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		rel = filepath.ToSlash(rel)

		// Directories are implied by their files' paths, but writing them keeps
		// empty directories and file modes intact across a restore.
		for _, parent := range parentDirs(rel) {
			if written[parent] {
				continue
			}
			written[parent] = true
			if err := writer.WriteHeader(&tar.Header{
				Typeflag: tar.TypeDir,
				Name:     parent + "/",
				Mode:     0755,
			}); err != nil {
				return err
			}
		}

		header, err := tar.FileInfoHeader(info, "")
		if err != nil {
			return err
		}
		header.Name = rel
		if err := writer.WriteHeader(header); err != nil {
			return err
		}

		file, err := os.Open(path)
		if err != nil {
			return err
		}
		if _, err := io.Copy(writer, file); err != nil {
			file.Close()
			return err
		}
		if err := file.Close(); err != nil {
			return err
		}
	}
	return nil
}

// parentDirs lists the intermediate directories of a tar path, outermost first.
func parentDirs(rel string) []string {
	parts := strings.Split(rel, "/")
	var dirs []string
	for i := 1; i < len(parts); i++ {
		dirs = append(dirs, strings.Join(parts[:i], "/"))
	}
	return dirs
}

// tarExtractor writes a restored tar stream into a directory, in the background,
// so a restore of a huge backup never buffers in memory.
type tarExtractor struct {
	writer *io.PipeWriter
	done   chan error
}

// newTarExtractor starts extracting into dest.
func newTarExtractor(dest string) *tarExtractor {
	reader, writer := io.Pipe()
	done := make(chan error, 1)
	go func() { done <- extractTar(reader, dest) }()
	return &tarExtractor{writer: writer, done: done}
}

// Write feeds the extractor.
func (e *tarExtractor) Write(p []byte) (int, error) { return e.writer.Write(p) }

// Close finishes the stream and reports whatever the extractor hit.
func (e *tarExtractor) Close() error {
	if err := e.writer.Close(); err != nil {
		return err
	}
	return <-e.done
}

// extractTar unpacks a tar stream into dest, refusing anything that would escape
// it. A restore is exactly the wrong moment to discover a path traversal.
func extractTar(reader io.Reader, dest string) error {
	if err := os.MkdirAll(dest, 0755); err != nil {
		return err
	}
	absDest, err := filepath.Abs(dest)
	if err != nil {
		return err
	}

	archive := tar.NewReader(reader)
	for {
		header, err := archive.Next()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}

		target := filepath.Join(absDest, filepath.FromSlash(header.Name))
		if target != absDest && !strings.HasPrefix(target, absDest+string(os.PathSeparator)) {
			return fmt.Errorf("refusing archive entry %q: it escapes the destination", header.Name)
		}

		switch header.Typeflag {
		case tar.TypeDir:
			// Mask to the permission bits before narrowing, so the value is <= 0777
			// and cannot overflow (gosec G115, AUDIT-L6).
			if err := os.MkdirAll(target, os.FileMode(header.Mode&int64(os.ModePerm))); err != nil { // #nosec G115 -- masked to 0777
				return err
			}
		case tar.TypeReg:
			if err := os.MkdirAll(filepath.Dir(target), 0755); err != nil {
				return err
			}
			file, err := os.OpenFile(target, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, os.FileMode(header.Mode&int64(os.ModePerm))) // #nosec G115 -- masked to 0777
			if err != nil {
				return err
			}
			if _, err := io.Copy(file, archive); err != nil {
				file.Close()
				return err
			}
			if err := file.Close(); err != nil {
				return err
			}
		default:
			// Symlinks, devices and other types are not sent in v1; skipping them
			// keeps a restore from creating something the sender never had.
			continue
		}
	}
}
