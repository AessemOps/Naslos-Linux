// Package buddy implements the Buddy Backup protocol: an instance exposes an
// authenticated volume that peers push encrypted, incremental backups to.
//
// The trust model is deliberately asymmetric:
//
//   - The **sender** holds the private key that authenticates it (ssh-style) and
//     the key encryption key (KEK) that wraps the per-backup data keys. Only it
//     can decrypt what it sent.
//   - The **receiver** holds only the sender's *public* key and stores opaque
//     ciphertext. It can account for space, list chains and prune them, but it
//     can never read a backup - that is the point of the feature.
//
// Keys are Ed25519 in OpenSSH form, so operators can use the tools they already
// know (`ssh-keygen -t ed25519`) and paste a public key as an authorized_keys
// line.
package buddy

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/crypto/ssh"
)

// IdentityVersion is the on-disk identity format version.
const IdentityVersion = 1

// Identity is the local instance's key material.
type Identity struct {
	Version int `json:"version"`
	// Name is a human label ("naslos-a"), never used for trust decisions.
	Name string `json:"name"`
	// PublicKey is the OpenSSH authorized_keys line peers authorize.
	PublicKey string `json:"publicKey"`
	// PrivateKey is an OpenSSH-encoded Ed25519 private key. It never leaves the
	// owner: peers only ever see PublicKey.
	PrivateKey string `json:"privateKey"`
	// KEK is the base64 32-byte key encryption key. It wraps each backup's data
	// key, so losing it means losing the ability to read the backups (documented
	// loudly in docs/buddy-backup.md).
	KEK       string    `json:"kek"`
	CreatedAt time.Time `json:"createdAt"`
}

// NewIdentity generates a fresh Ed25519 keypair and a random KEK.
func NewIdentity(name string) (*Identity, error) {
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, fmt.Errorf("generating key: %w", err)
	}

	block, err := ssh.MarshalPrivateKey(priv, "naslos-buddy "+name)
	if err != nil {
		return nil, fmt.Errorf("encoding private key: %w", err)
	}
	sshPub, err := ssh.NewPublicKey(pub)
	if err != nil {
		return nil, fmt.Errorf("encoding public key: %w", err)
	}

	kek := make([]byte, 32)
	if _, err := rand.Read(kek); err != nil {
		return nil, fmt.Errorf("generating key encryption key: %w", err)
	}

	return &Identity{
		Version:    IdentityVersion,
		Name:       name,
		PublicKey:  strings.TrimSpace(string(ssh.MarshalAuthorizedKey(sshPub))),
		PrivateKey: string(pem.EncodeToMemory(block)),
		KEK:        base64.StdEncoding.EncodeToString(kek),
		CreatedAt:  time.Now().UTC(),
	}, nil
}

// LoadIdentity reads an identity file.
func LoadIdentity(path string) (*Identity, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var id Identity
	if err := json.Unmarshal(data, &id); err != nil {
		return nil, fmt.Errorf("parsing identity %s: %w", path, err)
	}
	if id.PrivateKey == "" || id.PublicKey == "" {
		return nil, fmt.Errorf("identity %s is incomplete", path)
	}
	return &id, nil
}

// Save writes the identity with owner-only permissions (0600) and atomically, so
// a crash cannot leave a truncated key behind.
func (i *Identity) Save(path string) error {
	data, err := json.MarshalIndent(i, "", "  ")
	if err != nil {
		return err
	}
	if dir := filepath.Dir(path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0700); err != nil {
			return err
		}
	}

	tmp, err := os.CreateTemp(filepath.Dir(path), ".identity-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	// 0600 from the start: writing a private key with wider permissions, even
	// briefly, is not acceptable.
	if err := tmp.Chmod(0600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}

// privateKey decodes the OpenSSH private key.
func (i *Identity) privateKey() (ed25519.PrivateKey, error) {
	key, err := ssh.ParseRawPrivateKey([]byte(i.PrivateKey))
	if err != nil {
		return nil, fmt.Errorf("parsing private key: %w", err)
	}
	switch k := key.(type) {
	case ed25519.PrivateKey:
		return k, nil
	case *ed25519.PrivateKey:
		return *k, nil
	default:
		return nil, fmt.Errorf("unsupported key type %T: buddy keys are Ed25519", key)
	}
}

// Sign returns the raw Ed25519 signature of msg (callers base64-encode it).
func (i *Identity) Sign(msg []byte) ([]byte, error) {
	key, err := i.privateKey()
	if err != nil {
		return nil, err
	}
	return ed25519.Sign(key, msg), nil
}

// Fingerprint is the identity's key id (ssh-style "SHA256:…"): it addresses the
// sender's backups on a receiver and finds its authorized key.
func (i *Identity) Fingerprint() (string, error) {
	pub, _, _, _, err := ssh.ParseAuthorizedKey([]byte(i.PublicKey))
	if err != nil {
		return "", fmt.Errorf("parsing public key: %w", err)
	}
	return ssh.FingerprintSHA256(pub), nil
}

// EnvKEK is the environment variable that, when set, overrides the KEK stored in
// the identity file. Pointing it at a Kubernetes Secret keeps the data key
// material out of the identity file, which otherwise holds the signing key and
// the KEK together (PF-M14). It is base64 for 32 bytes, the same encoding as the
// file's `kek` field.
const EnvKEK = "BUDDY_KEK"

// KEKBytes decodes the key encryption key. A KEK in the environment (from a
// Secret) takes precedence over the one in the file, so an operator can move it
// out of the identity file without losing access.
func (i *Identity) KEKBytes() ([]byte, error) {
	source := strings.TrimSpace(i.KEK)
	if env := strings.TrimSpace(os.Getenv(EnvKEK)); env != "" {
		source = env
	}
	if source == "" {
		return nil, fmt.Errorf("no KEK: set %s (recommended) or restore the identity file's kek field", EnvKEK)
	}
	kek, err := base64.StdEncoding.DecodeString(source)
	if err != nil {
		return nil, fmt.Errorf("decoding kek: %w", err)
	}
	if len(kek) != 32 {
		return nil, fmt.Errorf("kek must be 32 bytes, got %d", len(kek))
	}
	return kek, nil
}

// PublicKeyOf parses an authorized_keys line (or a bare base64 blob) into the
// Ed25519 public key plus its fingerprint.
func PublicKeyOf(line string) (ed25519.PublicKey, string, error) {
	pub, _, _, _, err := ssh.ParseAuthorizedKey([]byte(strings.TrimSpace(line)))
	if err != nil {
		return nil, "", fmt.Errorf("parsing authorized key: %w", err)
	}
	cpk, ok := pub.(ssh.CryptoPublicKey)
	if !ok {
		return nil, "", fmt.Errorf("unsupported key type %s: buddy keys are Ed25519", pub.Type())
	}
	ed, ok := cpk.CryptoPublicKey().(ed25519.PublicKey)
	if !ok {
		return nil, "", fmt.Errorf("unsupported key type %s: buddy keys are Ed25519", pub.Type())
	}
	return ed, ssh.FingerprintSHA256(pub), nil
}

// Verify checks a signature made by the holder of the given authorized key.
func Verify(authorizedKey string, msg, signature []byte) error {
	pub, _, err := PublicKeyOf(authorizedKey)
	if err != nil {
		return err
	}
	if len(signature) != ed25519.SignatureSize {
		return fmt.Errorf("signature must be %d bytes, got %d", ed25519.SignatureSize, len(signature))
	}
	if !ed25519.Verify(pub, msg, signature) {
		return fmt.Errorf("signature verification failed")
	}
	return nil
}
