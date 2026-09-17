package buddy

import (
	"bytes"
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"time"
)

// Envelope parameters. The stream is cut into fixed-size chunks that are sealed
// independently: that is what makes a push resumable (a chunk can be re-sent on
// its own) and keeps memory bounded on both sides.
const (
	// EnvelopeVersion is the wire/format version.
	EnvelopeVersion = 1
	// ChunkPlainSize is the plaintext size of one sealed chunk.
	ChunkPlainSize = 1 << 20
	// MaxSealedChunkSize is what a receiver will accept for one chunk.
	MaxSealedChunkSize = ChunkPlainSize + 4096

	chunkMagic = "NBC1"
	aeadLabel  = "NB1"
)

// ManifestChunk is one sealed chunk as recorded in the manifest.
type ManifestChunk struct {
	Index       int `json:"index"`
	PlainBytes  int `json:"plainBytes"`
	SealedBytes int `json:"sealedBytes"`
	// Sha256Plain lets the owner verify the data after decryption. The receiver
	// cannot check it (it has no key) - the intended asymmetry.
	Sha256Plain string `json:"sha256Plain"`
}

// Manifest describes one pushed segment of a backup chain: what was sent, how it
// was encrypted, and how to put it back together. It is signed by the sender, so
// the owner can verify provenance long after the push, and a receiver tampering
// with metadata is detectable.
type Manifest struct {
	Version        int             `json:"version"`
	Source         string          `json:"source"`
	Chain          string          `json:"chain"`
	Kind           string          `json:"kind"` // "zfs-send" | "tar"
	FromSnapshot   string          `json:"fromSnapshot,omitempty"`
	ToSnapshot     string          `json:"toSnapshot,omitempty"`
	FromGUID       string          `json:"fromGUID,omitempty"`
	ToGUID         string          `json:"toGUID,omitempty"`
	CreatedAt      time.Time       `json:"createdAt"`
	KeyID          string          `json:"keyId"`
	StreamPrefix   string          `json:"streamPrefix"`
	ChunkPlainSize int             `json:"chunkPlainSize"`
	Chunks         []ManifestChunk `json:"chunks"`
	// DEKWrapped is the per-chain data key sealed with the owner's KEK: the
	// receiver stores it and cannot unwrap it.
	DEKWrapped string `json:"dekWrapped"`
	Signature  string `json:"signature,omitempty"`
}

// newStreamPrefix returns the 8-byte random prefix that keeps a chain's nonces
// unique even if the same DEK were ever reused.
func newStreamPrefix() ([]byte, error) {
	prefix := make([]byte, 8)
	if _, err := rand.Read(prefix); err != nil {
		return nil, fmt.Errorf("generating stream prefix: %w", err)
	}
	return prefix, nil
}

// chunkNonce is prefix || counter: unique per (DEK, index) by construction, which
// is the property AES-GCM needs for confidentiality.
func chunkNonce(prefix []byte, index int) []byte {
	nonce := make([]byte, 12)
	copy(nonce[:8], prefix)
	binary.BigEndian.PutUint32(nonce[8:], uint32(index))
	return nonce
}

// MaxChunkIndex is the largest chunk index the 32-bit nonce counter can carry.
// Beyond it the counter would wrap and two chunks would share a nonce, which is
// fatal for GCM - so an index past the limit is refused rather than truncated
// (NAS-021). At the 1 MiB chunk size this bounds one chain at 4 PiB.
const MaxChunkIndex = int64(1)<<32 - 1

// validateChunkIndex refuses an index the envelope cannot represent.
func validateChunkIndex(index int) error {
	if index < 0 || int64(index) > MaxChunkIndex {
		return fmt.Errorf("chunk index %d is outside 0..%d", index, MaxChunkIndex)
	}
	return nil
}

// chunkAAD binds a chunk to its position in the chain, so chunks cannot be
// reordered, swapped between chains, or truncated without detection.
func chunkAAD(source, chain string, index, plainLen int) []byte {
	return []byte(fmt.Sprintf("%s|%s|%s|%d|%d", aeadLabel, source, chain, index, plainLen))
}

// newDEK returns a fresh 256-bit data key for one chain segment.
func newDEK() ([]byte, error) {
	dek := make([]byte, 32)
	if _, err := rand.Read(dek); err != nil {
		return nil, fmt.Errorf("generating data key: %w", err)
	}
	return dek, nil
}

// aeadFor returns an AES-256-GCM AEAD, refusing any key that is not 256 bits.
func aeadFor(key []byte) (cipher.AEAD, error) {
	if len(key) != 32 {
		return nil, fmt.Errorf("AES-256-GCM needs a 32-byte key, got %d", len(key))
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	return cipher.NewGCM(block)
}

// digestOf is the hex SHA-256 of a slice.
func digestOf(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

// SealChunk encrypts one chunk and returns the self-describing blob plus the
// SHA-256 of the plaintext (recorded in the manifest for later verification).
//
// Layout: magic(4) | plainLen(4, BE) | nonce(12) | ciphertext+tag(16).
func SealChunk(dek, prefix []byte, source, chain string, index int, plain []byte) ([]byte, string, error) {
	if err := validateChunkIndex(index); err != nil {
		return nil, "", err
	}
	aead, err := aeadFor(dek)
	if err != nil {
		return nil, "", err
	}

	nonce := chunkNonce(prefix, index)
	sealed := aead.Seal(nil, nonce, plain, chunkAAD(source, chain, index, len(plain)))

	var buf bytes.Buffer
	buf.WriteString(chunkMagic)
	_ = binary.Write(&buf, binary.BigEndian, uint32(len(plain)))
	buf.Write(nonce)
	buf.Write(sealed)

	return buf.Bytes(), digestOf(plain), nil
}

// OpenChunk decrypts a sealed blob, failing on tampering, on a chunk stored under
// the wrong index, and on truncation.
func OpenChunk(dek []byte, source, chain string, index int, sealed []byte) ([]byte, error) {
	if err := validateChunkIndex(index); err != nil {
		return nil, err
	}
	if len(sealed) < len(chunkMagic)+4+12 {
		return nil, fmt.Errorf("chunk is too short to be a valid envelope")
	}
	if string(sealed[:4]) != chunkMagic {
		return nil, fmt.Errorf("chunk is not a %s envelope", chunkMagic)
	}

	plainLen := int(binary.BigEndian.Uint32(sealed[4:8]))
	if plainLen < 0 || plainLen > ChunkPlainSize {
		return nil, fmt.Errorf("chunk declares an impossible plaintext size (%d)", plainLen)
	}

	// The nonce must be exactly the one this index would produce: that catches a
	// chunk filed under the wrong index or moved between chains.
	prefix := sealed[8:16]
	if !bytes.Equal(sealed[8:20], chunkNonce(prefix, index)) {
		return nil, fmt.Errorf("chunk %d carries a mismatched nonce: it belongs to another index or chain", index)
	}

	aead, err := aeadFor(dek)
	if err != nil {
		return nil, err
	}
	plain, err := aead.Open(nil, sealed[8:20], sealed[20:], chunkAAD(source, chain, index, plainLen))
	if err != nil {
		return nil, fmt.Errorf("chunk %d failed authentication (wrong key, or the data was altered): %w", index, err)
	}
	if len(plain) != plainLen {
		return nil, fmt.Errorf("chunk %d decrypted to %d bytes, expected %d", index, len(plain), plainLen)
	}
	return plain, nil
}

// WrapDEK seals a chain's data key with the owner's KEK. The wrapped form travels
// in the manifest, so restoring needs only the owner's KEK - never the receiver's
// cooperation beyond handing the bytes back.
func WrapDEK(kek, dek []byte, source, chain string) (string, error) {
	aead, err := aeadFor(kek)
	if err != nil {
		return "", err
	}
	nonce := make([]byte, 12)
	if _, err := rand.Read(nonce); err != nil {
		return "", fmt.Errorf("generating wrap nonce: %w", err)
	}
	sealed := aead.Seal(nil, nonce, dek, []byte(fmt.Sprintf("%s|dek|%s|%s", aeadLabel, source, chain)))
	return base64.StdEncoding.EncodeToString(append(nonce, sealed...)), nil
}

// UnwrapDEK opens a wrapped data key with the owner's KEK.
func UnwrapDEK(kek []byte, wrapped, source, chain string) ([]byte, error) {
	raw, err := base64.StdEncoding.DecodeString(wrapped)
	if err != nil {
		return nil, fmt.Errorf("decoding wrapped key: %w", err)
	}
	if len(raw) < 12+32+16 {
		return nil, fmt.Errorf("wrapped key is too short")
	}
	aead, err := aeadFor(kek)
	if err != nil {
		return nil, err
	}
	dek, err := aead.Open(nil, raw[:12], raw[12:], []byte(fmt.Sprintf("%s|dek|%s|%s", aeadLabel, source, chain)))
	if err != nil {
		return nil, fmt.Errorf("cannot unwrap the data key: this backup was not made with this key (%w)", err)
	}
	return dek, nil
}

// canonical is the manifest's signed form: everything except the signature.
func (m *Manifest) canonical() ([]byte, error) {
	copyOf := *m
	copyOf.Signature = ""
	return json.Marshal(copyOf)
}

// Sign signs the manifest and stamps it with the sender's key id.
func (m *Manifest) Sign(id *Identity) error {
	keyID, err := id.Fingerprint()
	if err != nil {
		return err
	}
	m.KeyID = keyID

	canonical, err := m.canonical()
	if err != nil {
		return err
	}
	sig, err := id.Sign(canonical)
	if err != nil {
		return err
	}
	m.Signature = base64.StdEncoding.EncodeToString(sig)
	return nil
}

// VerifySignature checks the manifest was signed by the holder of the authorized
// key, and that the key matches the manifest's own key id.
func (m *Manifest) VerifySignature(authorizedKey string) error {
	if m.Signature == "" {
		return fmt.Errorf("manifest is not signed")
	}
	_, fingerprint, err := PublicKeyOf(authorizedKey)
	if err != nil {
		return err
	}
	if m.KeyID != fingerprint {
		return fmt.Errorf("manifest was signed by %s but claims %s", fingerprint, m.KeyID)
	}
	sig, err := base64.StdEncoding.DecodeString(m.Signature)
	if err != nil {
		return fmt.Errorf("decoding manifest signature: %w", err)
	}
	canonical, err := m.canonical()
	if err != nil {
		return err
	}
	return Verify(authorizedKey, canonical, sig)
}

// validateManifestShape checks the sender-controlled parts of a manifest that
// later drive restores and pruning. The receiver cannot decrypt, so this is the
// only place it can refuse metadata that is malformed or self-inconsistent
// (NAS-012): a duplicate or out-of-order index, an impossible chunk size, a
// missing stream prefix or key, or an unknown payload kind.
func validateManifestShape(m *Manifest) error {
	if m.Kind != "zfs-send" && m.Kind != "tar" {
		return fmt.Errorf("unsupported payload kind %q", m.Kind)
	}
	if m.CreatedAt.IsZero() {
		return fmt.Errorf("manifest has no creation time")
	}
	if m.ChunkPlainSize != ChunkPlainSize {
		return fmt.Errorf("chunk plain size is %d, this receiver stores %d-byte chunks", m.ChunkPlainSize, ChunkPlainSize)
	}
	prefix, err := base64.StdEncoding.DecodeString(m.StreamPrefix)
	if err != nil || len(prefix) != 8 {
		return fmt.Errorf("stream prefix must be base64 for 8 bytes")
	}
	if m.DEKWrapped == "" {
		return fmt.Errorf("manifest carries no wrapped data key")
	}
	if _, err := base64.StdEncoding.DecodeString(m.DEKWrapped); err != nil {
		return fmt.Errorf("wrapped data key is not valid base64")
	}

	for i, chunk := range m.Chunks {
		if chunk.Index != i {
			return fmt.Errorf("chunk %d is out of order (index %d): the manifest must list chunks 0..n-1 exactly once", i, chunk.Index)
		}
		if chunk.PlainBytes <= 0 || chunk.PlainBytes > ChunkPlainSize {
			return fmt.Errorf("chunk %d declares %d plain bytes, outside (0, %d]", i, chunk.PlainBytes, ChunkPlainSize)
		}
		if chunk.SealedBytes <= chunk.PlainBytes || chunk.SealedBytes > MaxSealedChunkSize {
			return fmt.Errorf("chunk %d declares %d sealed bytes, which cannot hold %d plain bytes",
				i, chunk.SealedBytes, chunk.PlainBytes)
		}
		if len(chunk.Sha256Plain) != 64 || !isHex(chunk.Sha256Plain) {
			return fmt.Errorf("chunk %d has an invalid plaintext digest", i)
		}
	}
	return nil
}

// isHex reports whether s is entirely hexadecimal characters.
func isHex(s string) bool {
	for _, r := range s {
		if (r < '0' || r > '9') && (r < 'a' || r > 'f') && (r < 'A' || r > 'F') {
			return false
		}
	}
	return true
}
