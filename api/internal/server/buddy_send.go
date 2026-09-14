package server

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/agent"
	"github.com/AessemOps/Naslos-Linux/api/internal/buddy"
)

// Instance-side sender and restore (FR-BUD-11…14).
//
// Up to here an instance could only *receive* pushes; sending required an operator
// with `buddyctl` on the box. These handlers close that gap by combining the two
// halves the API already owns: the agent (which can stream `zfs send`/`zfs receive`
// on the host) and the buddy client (which encrypts and uploads). Nothing is
// buffered, so a multi-terabyte dataset is never held in memory, and the stream is
// checked against the agent's own estimate before the manifest is published.

// defaultBuddyIdentity is where this instance's key material lives. It sits beside
// the other Naslos state files, on the persistent volume.
const defaultBuddyIdentity = "/var/lib/naslos/buddy-identity.json"

// buddyIdentityPath is the configured identity file.
func buddyIdentityPath() string {
	if env := os.Getenv("BUDDY_IDENTITY"); env != "" {
		return env
	}
	return defaultBuddyIdentity
}

// loadBuddyIdentity reads this instance's identity, or explains how to make one.
func loadBuddyIdentity() (*buddy.Identity, error) {
	path := buddyIdentityPath()
	identity, err := buddy.LoadIdentity(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("this instance has no backup identity yet: create one with POST /api/buddy/identity " +
				"(its public key is what a buddy authorizes)")
		}
		return nil, err
	}
	return identity, nil
}

// handleBuddyIdentity reports this instance's public key, or creates the keypair on
// first use:
//
//	GET  /api/buddy/identity          → who this instance is to its buddies
//	POST /api/buddy/identity {name, replace}
//
// The private key and the key encryption key never leave the file and are never
// returned; the public key is what an operator hands to a buddy.
func (s *Server) handleBuddyIdentity(w http.ResponseWriter, req *http.Request) {
	if !s.requireBuddyAdminAuth(w, req) {
		return
	}

	path := buddyIdentityPath()
	switch req.Method {
	case http.MethodGet:
		identity, err := buddy.LoadIdentity(path)
		if err != nil {
			if os.IsNotExist(err) {
				writeJSON(w, http.StatusOK, map[string]any{
					"exists": false,
					"path":   path,
					"help":   "create one with POST /api/buddy/identity; back the file up, losing it means losing the ability to read your backups",
				})
				return
			}
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		fingerprint, err := identity.Fingerprint()
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{
			"exists":      true,
			"name":        identity.Name,
			"publicKey":   identity.PublicKey,
			"fingerprint": fingerprint,
			"path":        path,
			"createdAt":   identity.CreatedAt,
		})

	case http.MethodPost:
		var request struct {
			Name    string `json:"name"`
			Replace bool   `json:"replace"`
		}
		if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil && err != io.EOF {
			writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
			return
		}

		if _, err := os.Stat(path); err == nil && !request.Replace {
			// Replacing an identity orphans every backup this instance ever made
			// (the data keys can no longer be unwrapped), so it takes an explicit
			// flag - and the caller is told what it costs.
			writeError(w, http.StatusConflict,
				"an identity already exists at "+path+": it is the only key that can read this instance's backups, "+
					"so replacing it loses them. Send {\"replace\": true} if that is really what you want.")
			return
		}

		name := strings.TrimSpace(request.Name)
		if name == "" {
			host, _ := os.Hostname()
			name = strings.ToLower(strings.TrimSpace(host))
		}
		if name == "" {
			name = "naslos"
		}

		identity, err := buddy.NewIdentity(name)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		if err := identity.Save(path); err != nil {
			writeError(w, http.StatusInternalServerError, "saving the identity: "+err.Error())
			return
		}

		fingerprint, err := identity.Fingerprint()
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		log.Printf("buddy: created a backup identity for %q (%s) at %s", identity.Name, fingerprint, path)
		writeJSON(w, http.StatusCreated, map[string]any{
			"exists":      true,
			"name":        identity.Name,
			"publicKey":   identity.PublicKey,
			"fingerprint": fingerprint,
			"path":        path,
			"warning":     "back this file up somewhere your buddies cannot reach: it holds the only key that can read your backups",
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// sendShortfallAllowance is how far below the dry run's estimate a complete stream
// may land, before we call the stream truncated.
//
// `zfs send -nP` reports the payload, not the stream's framing records, and the
// relationship is not a strict floor: measured on the VM, a full send of 44,376 bytes
// came in 232 bytes *above* its estimate while a 53 MB full send was 9,712 above, but
// an incremental send of 56.9 MB came in 119,688 bytes *below* it. So the estimate is
// an approximation, and the allowance has to tolerate a percent in either direction.
//
// What this guard is for: a `zfs send` that died loses everything after the kill, so
// the shortfall is normally huge. Anything smaller is caught where it matters - ZFS
// refuses a truncated stream at `zfs receive` (the stream carries its own end record
// and per-record checksums) - and the sender can simply retry, which resumes the
// chain. It is an early warning, not the integrity boundary.
func sendShortfallAllowance(estimate int64) int64 {
	const floor = 1 << 20 // 1 MiB
	relative := estimate / 100
	if relative > floor {
		return relative
	}
	return floor
}

// instanceSendState is what an interrupted instance-side send needs to continue: the
// buddy chain (its data key and nonce prefix) plus the snapshot pair and GUIDs the
// stream was made from. Keeping all of it means a retry produces the *same* bytes,
// so the buddy skips what already arrived instead of storing a second copy.
type instanceSendState struct {
	Chain        buddy.ChainState `json:"chain"`
	Receiver     string           `json:"receiver"`
	Source       string           `json:"source"`
	Dataset      string           `json:"dataset"`
	ToSnapshot   string           `json:"toSnapshot"`
	FromSnapshot string           `json:"fromSnapshot"`
	FromGUID     string           `json:"fromGUID"`
	ToGUID       string           `json:"toGUID"`
}

// randomSuffix returns four hex characters so two sends started in the same second
// still get distinct snapshot names.
func randomSuffix() string {
	buf := make([]byte, 2)
	if _, err := rand.Read(buf); err != nil {
		return fmt.Sprintf("%04x", time.Now().UnixNano()&0xffff)
	}
	return hex.EncodeToString(buf)
}

// sendStateDir is where interrupted sends keep their state, beside the identity.
func sendStateDir() string {
	return filepath.Join(filepath.Dir(buddyIdentityPath()), "buddy-sends")
}

// sendStateName names the state file for a (buddy, source) pair.
func sendStateName(receiverURL, source string) string {
	sum := sha256.Sum256([]byte(receiverURL + "|" + source))
	return hex.EncodeToString(sum[:16]) + ".json"
}

// saveSendState writes the resume state with owner-only permissions: it carries a
// chain data key.
func saveSendState(path string, state *instanceSendState) error {
	data, err := json.MarshalIndent(state, "", "  ")
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".send-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if err := tmp.Chmod(0600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}

// loadSendState reads a resume state, if one exists.
func loadSendState(path string) (*instanceSendState, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var state instanceSendState
	if err := json.Unmarshal(data, &state); err != nil {
		return nil, err
	}
	return &state, nil
}

type buddySendRequest struct {
	// Dataset is the local ZFS dataset to back up.
	Dataset string `json:"dataset"`
	// Source is the name it is stored under on the buddy.
	Source string `json:"source"`
	// Receiver is the buddy's base URL.
	Receiver string `json:"receiver"`
	// PruneKeep, when set, keeps only the newest N chains on the buddy.
	PruneKeep int `json:"pruneKeep"`
	// Raw sends the encrypted records (-w). Default true: the buddy should never
	// need to decrypt anything.
	Raw *bool `json:"raw"`
	// Force sends a full stream even when an incremental base is available.
	Force bool `json:"force"`
}

// normalizeReceiverURL validates a buddy URL. Peers are reached over the network
// the operator already exposes, so anything but http/https is a mistake worth
// refusing here rather than inside the client.
func normalizeReceiverURL(raw string) (string, error) {
	trimmed := strings.TrimSpace(raw)
	if trimmed == "" {
		return "", fmt.Errorf("receiver is required: it is the buddy's base URL")
	}
	parsed, err := url.Parse(trimmed)
	if err != nil {
		return "", fmt.Errorf("invalid receiver URL: %w", err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return "", fmt.Errorf("receiver URL %q must start with http:// or https://", trimmed)
	}
	if parsed.Host == "" {
		return "", fmt.Errorf("receiver URL %q has no host", trimmed)
	}
	return strings.TrimRight(trimmed, "/"), nil
}

// handleBuddySendSyncLegacy is the pre-jobs synchronous send, kept for reference
// during the async migration. It is no longer routed; POST /api/buddy/send is
// served by handleBuddySend in buddy_jobs.go.
func (s *Server) handleBuddySendSyncLegacy(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if !s.requireBuddyAdminAuth(w, req) {
		return
	}
	if s.agent == nil {
		writeError(w, http.StatusServiceUnavailable, "the API has no agent client, so it cannot stream ZFS data")
		return
	}

	var request buddySendRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
		return
	}

	dataset := strings.TrimSpace(request.Dataset)
	source := strings.TrimSpace(request.Source)
	if dataset == "" || source == "" {
		writeError(w, http.StatusBadRequest, "dataset and source are required")
		return
	}
	if err := buddy.ValidateSource(source); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	receiverURL, err := normalizeReceiverURL(request.Receiver)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	identity, err := loadBuddyIdentity()
	if err != nil {
		writeError(w, http.StatusPreconditionFailed, err.Error())
		return
	}
	client := buddy.NewClient(receiverURL, identity)

	// The request context bounds the whole send: if the operator's client goes
	// away, the agent's `zfs send` is killed with it instead of running on.
	ctx := req.Context()

	raw := true
	if request.Raw != nil {
		raw = *request.Raw
	}

	// Find the base: the buddy's manifest records the GUID of the snapshot it was
	// given, so an incremental send is possible even if that snapshot was renamed
	// since. Without a match this is a full send.
	//
	// An interrupted send is different: it must repeat the *same* stream, so its
	// base and snapshot come from its own state file rather than from a fresh
	// decision about what to send.
	statePath := filepath.Join(sendStateDir(), sendStateName(receiverURL, source))
	var state *instanceSendState
	wasResume := false
	if !request.Force {
		if loaded, err := loadSendState(statePath); err == nil &&
			loaded.Receiver == receiverURL && loaded.Source == source && loaded.Dataset == dataset {
			if snapshots, err := s.agent.SnapshotsWithGUID(ctx, dataset); err == nil {
				for _, snapshot := range snapshots {
					if snapshot.Name == loaded.ToSnapshot {
						state = loaded
						wasResume = true
						break
					}
				}
			}
			if state == nil {
				// The snapshot the stream was made from is gone (someone destroyed
				// it): that state cannot be continued.
				log.Printf("buddy send: discarding the resume state for %s: snapshot %s no longer exists", source, loaded.ToSnapshot)
				_ = os.Remove(statePath)
			}
		}
	}

	base, baseGUID := "", ""
	if state != nil {
		base, baseGUID = state.FromSnapshot, state.FromGUID
	} else if !request.Force {
		previous, err := client.Manifest(source, "")
		if err == nil && previous.Kind == "zfs-send" && previous.ToGUID != "" {
			if snapshots, err := s.agent.SnapshotsWithGUID(ctx, dataset); err == nil {
				for _, snapshot := range snapshots {
					if snapshot.GUID == previous.ToGUID {
						base, baseGUID = snapshot.Name, snapshot.GUID
						break
					}
				}
			}
		} else if err != nil {
			log.Printf("buddy send: no previous backup of %s on %s (%v): sending a full stream", source, receiverURL, err)
		}
	}

	// A fresh attempt snapshots now; a resumed one reuses the snapshot it started
	// with, which is what makes the stream identical.
	// A second-granular name collided when two sends started within the same second
	// (`zfs snapshot` refuses a name that already exists), so each attempt gets a
	// short random suffix as well.
	snapshot := "buddy-" + time.Now().UTC().Format("20060102T150405Z") + "-" + randomSuffix()
	targetGUID := ""
	if state != nil {
		snapshot, targetGUID = state.ToSnapshot, state.ToGUID
	}
	if state == nil {
		if err := s.agent.CreateSnapshot(ctx, dataset, snapshot); err != nil {
			writeError(w, http.StatusBadGateway, "snapshotting "+dataset+": "+err.Error())
			return
		}
		// The new snapshot's GUID goes into the manifest so the *next* send can find
		// it again and stay incremental.
		if snapshots, err := s.agent.SnapshotsWithGUID(ctx, dataset); err == nil {
			for _, info := range snapshots {
				if info.Name == snapshot {
					targetGUID = info.GUID
				}
			}
		}
		state = &instanceSendState{
			Receiver:     receiverURL,
			Source:       source,
			Dataset:      dataset,
			ToSnapshot:   snapshot,
			FromSnapshot: base,
			FromGUID:     baseGUID,
			ToGUID:       targetGUID,
		}
		if chain, err := buddy.NewChainState(source, "zfs-send"); err != nil {
			writeError(w, http.StatusInternalServerError, "creating the chain state: "+err.Error())
			return
		} else {
			state.Chain = *chain
		}
	}
	// The state is written before the first chunk leaves: a crash, a disconnect or
	// a killed send must still be resumable, and the file carries the chain's data
	// key, so it is written 0600 beside the identity.
	if err := saveSendState(statePath, state); err != nil {
		writeError(w, http.StatusInternalServerError, "saving the resume state: "+err.Error())
		return
	}

	sendOptions := agent.SendStreamOptions{Dataset: dataset, To: snapshot, From: base, Raw: raw}
	expected, err := s.agent.EstimateSend(ctx, sendOptions)
	if err != nil {
		// An estimate is a nicety, not a requirement: without it the send still
		// works, there is just no number to check completeness against.
		log.Printf("buddy send: could not estimate %s@%s: %v", dataset, snapshot, err)
	}

	stream, err := s.agent.SendStream(ctx, sendOptions)
	if err != nil {
		writeError(w, http.StatusBadGateway, "starting zfs send: "+err.Error())
		return
	}
	defer stream.Close()

	started := time.Now()
	chainState := state.Chain
	result, err := client.Push(buddy.PushOptions{
		Source:       source,
		Kind:         "zfs-send",
		Reader:       stream,
		State:        &chainState,
		FromSnapshot: base,
		ToSnapshot:   snapshot,
		FromGUID:     baseGUID,
		ToGUID:       targetGUID,
		PruneKeep:    request.PruneKeep,
		BeforePublish: func(pushed *buddy.PushResult) error {
			if expected > 0 && pushed.PlainBytes+sendShortfallAllowance(expected) < expected {
				return fmt.Errorf("the send stream ended early (%d of at least %d bytes), so nothing was published. "+
					"Retry the same send to continue chain %s: the buddy skips the chunks it already has",
					pushed.PlainBytes, expected, chainState.Chain)
			}
			return nil
		},
	})
	if err != nil {
		writeError(w, http.StatusBadGateway, err.Error())
		return
	}

	// Published: the chain is complete and resumable no longer means anything.
	_ = os.Remove(statePath)

	log.Printf("buddy send: %s@%s -> %s %s (%d chunks, %d bytes, incremental=%v, resumed=%v)",
		dataset, snapshot, receiverURL, source, result.Chunks, result.PlainBytes, base != "", wasResume)
	writeJSON(w, http.StatusOK, map[string]any{
		"status":          "backed up",
		"dataset":         dataset,
		"source":          source,
		"receiver":        receiverURL,
		"snapshot":        snapshot,
		"base":            base,
		"baseGUID":        baseGUID,
		"toGUID":          targetGUID,
		"incremental":     base != "",
		"resumed":         wasResume,
		"raw":             raw,
		"chain":           result.Chain,
		"chunks":          result.Chunks,
		"uploaded":        result.Uploaded,
		"skipped":         result.Skipped,
		"plainBytes":      result.PlainBytes,
		"sealedBytes":     result.SealedBytes,
		"estimatedBytes":  expected,
		"prunedChains":    result.PrunedChains,
		"durationSeconds": time.Since(started).Seconds(),
	})
}

// buddyRestoreRequest is a restore back onto this instance's own ZFS.
type buddyRestoreRequest struct {
	// Source is the backup's logical name on the buddy.
	Source string `json:"source"`
	// Receiver is the buddy's base URL (where the backup lives).
	Receiver string `json:"receiver"`
	// Dataset is the local destination (`pool/name`). Not needed for verify.
	Dataset string `json:"dataset"`
	// Chain restores an older chain instead of the current one.
	Chain string `json:"chain"`
	// Force applies `zfs receive -F`: roll the destination back to the stream's
	// snapshot instead of refusing when it already has one.
	Force bool `json:"force"`
	// Verify decrypts and hashes the stream without touching ZFS: the proof that a
	// backup is intact, and the check a restore drill can compare with the node.
	Verify bool `json:"verify"`
}

// streamDigest is the result of a verify run.
type streamDigest struct {
	Chain  string `json:"chain"`
	SHA256 string `json:"sha256"`
	Bytes  int64  `json:"bytes"`
}

// handleBuddyRestore pulls a backup back and writes it into ZFS:
//
//	POST /api/buddy/restore {"source":"naslos-a/test","receiver":"https://buddy","dataset":"test/restored"}
//
// The stream is authenticated before ZFS sees it: the buddy client verifies the
// manifest signature, the chain's contiguity and every chunk's digest, and ZFS then
// refuses anything truncated. A restore either lands whole or fails loudly.
func (s *Server) handleBuddyRestore(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if !s.requireBuddyAdminAuth(w, req) {
		return
	}
	if s.agent == nil {
		writeError(w, http.StatusServiceUnavailable, "the API has no agent client, so it cannot stream ZFS data")
		return
	}

	var request buddyRestoreRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, req.Body, 64<<10)).Decode(&request); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
		return
	}

	source := strings.TrimSpace(request.Source)
	dataset := strings.TrimSpace(request.Dataset)
	if source == "" {
		writeError(w, http.StatusBadRequest, "source is required")
		return
	}
	if dataset == "" && !request.Verify {
		writeError(w, http.StatusBadRequest, "dataset is required (it is where the backup is restored to)")
		return
	}
	if err := buddy.ValidateSource(source); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	receiverURL, err := normalizeReceiverURL(request.Receiver)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	identity, err := loadBuddyIdentity()
	if err != nil {
		writeError(w, http.StatusPreconditionFailed, err.Error())
		return
	}
	client := buddy.NewClient(receiverURL, identity)
	ctx := req.Context()

	// A restore is usually a *sequence*: the newest backup is normally an
	// incremental, and ZFS refuses an incremental stream whose base is missing. So
	// the chains are worked out first (oldest full send, then each incremental) and
	// applied in that order - which is the only order that can succeed.
	sequence, err := client.RestoreSequence(source, request.Chain)
	if err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}

	started := time.Now()

	// Verify: decrypt and hash every chain of the sequence without touching ZFS.
	// This is the cheap half of a restore drill - and the half that proves the
	// bytes are intact - so it is worth having as an operation of its own.
	if request.Verify {
		digests := make([]streamDigest, 0, len(sequence))
		var total int64
		for _, chain := range sequence {
			hasher := sha256.New()
			result, err := client.Restore(buddy.RestoreOptions{
				Source: source,
				Chain:  chain.Chain,
				Out:    hasher,
			})
			if err != nil {
				writeError(w, http.StatusBadGateway, err.Error())
				return
			}
			digests = append(digests, streamDigest{
				Chain:  chain.Chain,
				SHA256: hex.EncodeToString(hasher.Sum(nil)),
				Bytes:  result.PlainBytes,
			})
			total += result.PlainBytes
		}

		log.Printf("buddy verify: %s %s (%d chain(s), %d bytes)", receiverURL, source, len(digests), total)
		writeJSON(w, http.StatusOK, map[string]any{
			"status":          "verified",
			"source":          source,
			"receiver":        receiverURL,
			"chains":          digests,
			"plainBytes":      total,
			"durationSeconds": time.Since(started).Seconds(),
		})
		return
	}

	var (
		totalChunks int
		totalBytes  int64
		applied     = make([]string, 0, len(sequence))
	)
	for i, chain := range sequence {
		// -F only on the first stream: later ones are incrementals that must not be
		// forced, or the destination would be rolled back between them.
		force := request.Force && i == 0
		writer, wait, err := s.agent.ReceiveStream(ctx, dataset, force)
		if err != nil {
			writeError(w, http.StatusBadGateway, "starting zfs receive: "+err.Error())
			return
		}

		result, err := client.Restore(buddy.RestoreOptions{
			Source: source,
			Chain:  chain.Chain,
			Out:    writer,
		})
		if err != nil {
			// Close the agent's stream and collect whatever ZFS said about the
			// partial data; the error from the buddy side is the useful one.
			_ = writer.Close()
			_ = wait()
			writeError(w, http.StatusBadGateway, err.Error())
			return
		}
		// The writer must be closed first: that is what tells `zfs receive` the
		// stream ended, and only then can it finish the dataset.
		if err := wait(); err != nil {
			writeError(w, http.StatusUnprocessableEntity, fmt.Sprintf(
				"zfs receive refused chain %s: %s", chain.Chain, err.Error()))
			return
		}

		applied = append(applied, chain.Chain)
		totalChunks += result.Chunks
		totalBytes += result.PlainBytes
	}

	last := sequence[len(sequence)-1]
	log.Printf("buddy restore: %s %s -> %s (%d chain(s), %d chunks, %d bytes)",
		receiverURL, source, dataset, len(applied), totalChunks, totalBytes)

	writeJSON(w, http.StatusOK, map[string]any{
		"status":          "restored",
		"source":          source,
		"dataset":         dataset,
		"chains":          applied,
		"chain":           last.Chain,
		"incrementalFrom": last.FromSnapshot,
		"snapshot":        last.ToSnapshot,
		"chunks":          totalChunks,
		"plainBytes":      totalBytes,
		"kind":            last.Kind,
		"takenAt":         last.CreatedAt,
		"durationSeconds": time.Since(started).Seconds(),
	})
}
