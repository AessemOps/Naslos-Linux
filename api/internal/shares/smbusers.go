// SMB account synchronisation.
//
// Naslos keeps one identity (OpenLDAP) and mirrors it into Samba's passdb so
// the same user name and password work for the web UI and for SMB shares.
// LDAP password hashes are salted and cannot yield an NT hash, so the NT hash
// is captured at password-change time (api/internal/identity computes it from
// the plaintext the user submitted) and recorded here.
//
// The accounts are rendered into an `smbpasswd`-format file which the
// privileged agent writes next to smb.conf on the node; the naslos-samba
// container imports it with `pdbedit -i smbpasswd:<file>` (see
// samba/image/entrypoint.sh). Because the import carries the NT hash, no
// interactive tooling ever runs against the node.
package shares

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// noLMHash is the placeholder Samba uses for "no LAN Manager hash", which is
// the modern default (LM is cryptographically broken and disabled).
const noLMHash = "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"

// SambaUser is an SMB-visible account mirrored from an LDAP user.
type SambaUser struct {
	// UID is the LDAP uid / Samba user name.
	UID string `json:"uid"`
	// UIDNumber is the POSIX uid, taken from the LDAP entry so the Samba and
	// LDAP views of the account agree.
	UIDNumber int `json:"uidNumber"`
	// NTHash is the uppercase hex MD4(UTF-16LE(password)).
	NTHash string `json:"ntHash"`
	// PasswordSetAt is when the hash was recorded, rendered as Samba's LCT.
	PasswordSetAt time.Time `json:"passwordSetAt"`
	// Enabled mirrors the LDAP account state (shadowExpire).
	Enabled bool `json:"enabled"`
}

// SambaUserStore persists the SMB account mirror.
type SambaUserStore struct {
	path  string
	users map[string]*SambaUser
}

// NewSambaUserStore loads the store from path. An empty path keeps it in
// memory only (used by tests).
func NewSambaUserStore(path string) *SambaUserStore {
	s := &SambaUserStore{path: path, users: make(map[string]*SambaUser)}
	s.load()
	return s
}

func (s *SambaUserStore) load() {
	if s.path == "" {
		return
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		return
	}
	var users []*SambaUser
	if err := json.Unmarshal(data, &users); err != nil {
		return
	}
	for _, u := range users {
		if u != nil && u.UID != "" {
			s.users[u.UID] = u
		}
	}
}
func (s *SambaUserStore) save() error {
	if s.path == "" {
		return nil
	}
	data, err := json.MarshalIndent(s.List(), "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling SMB accounts: %w", err)
	}
	data = append(data, '\n')

	if dir := filepath.Dir(s.path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("creating SMB account directory: %w", err)
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), ".smbusers-*.tmp")
	if err != nil {
		return fmt.Errorf("creating temp SMB account file: %w", err)
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("writing SMB accounts: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("closing SMB accounts: %w", err)
	}
	// 0600: the file holds NT hashes.
	if err := os.Chmod(tmpName, 0600); err != nil {
		return fmt.Errorf("setting SMB account file mode: %w", err)
	}
	if err := os.Rename(tmpName, s.path); err != nil {
		return fmt.Errorf("replacing SMB accounts: %w", err)
	}
	return nil
}

// List returns the accounts ordered by uid.
func (s *SambaUserStore) List() []*SambaUser {
	out := make([]*SambaUser, 0, len(s.users))
	for _, u := range s.users {
		out = append(out, u)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].UID < out[j].UID })
	return out
}

// Get returns an account by uid.
func (s *SambaUserStore) Get(uid string) (*SambaUser, bool) {
	u, ok := s.users[uid]
	return u, ok
}

// Upsert records (or updates) the NT hash for uid. uidNumber of 0 leaves any
// previously known uid untouched.
func (s *SambaUserStore) Upsert(uid string, uidNumber int, ntHash string) error {
	uid = strings.TrimSpace(uid)
	if uid == "" {
		return fmt.Errorf("uid is required")
	}
	if ntHash == "" {
		return fmt.Errorf("NT hash is required for %s", uid)
	}

	if existing, ok := s.users[uid]; ok {
		existing.NTHash = strings.ToUpper(ntHash)
		existing.PasswordSetAt = time.Now().UTC()
		if uidNumber > 0 {
			existing.UIDNumber = uidNumber
		}
	} else {
		s.users[uid] = &SambaUser{
			UID:           uid,
			UIDNumber:     uidNumber,
			NTHash:        strings.ToUpper(ntHash),
			PasswordSetAt: time.Now().UTC(),
			Enabled:       true,
		}
	}
	return s.save()
}

// SetEnabled mirrors an LDAP enable/disable into the SMB account.
func (s *SambaUserStore) SetEnabled(uid string, enabled bool) error {
	u, ok := s.users[uid]
	if !ok {
		// Nothing to mirror yet: the account is created on first password set.
		return nil
	}
	u.Enabled = enabled
	return s.save()
}

// Remove drops an account so deleting an LDAP user removes SMB access too.
func (s *SambaUserStore) Remove(uid string) error {
	if _, ok := s.users[uid]; !ok {
		return nil
	}
	delete(s.users, uid)
	return s.save()
}

// Count returns the number of mirrored accounts.
func (s *SambaUserStore) Count() int {
	return len(s.users)
}

// RenderSMBPasswd renders the accounts in Samba's smbpasswd format, which
// `pdbedit -i smbpasswd:<file>` consumes:
//
//	username:uid:LMHASH:NTHASH:[U          ]:LCT-<hex unix time>:
//
// Disabled accounts keep their hash but carry the disabled flag, so
// re-enabling restores access without needing the password again.
func (s *SambaUserStore) RenderSMBPasswd() string {
	var sb strings.Builder
	sb.WriteString("# Generated by Naslos - do not edit.\n")
	sb.WriteString("# NT hashes are synced from LDAP password changes; imported with\n")
	sb.WriteString("# `pdbedit -i smbpasswd:<this file>` by the naslos-samba container.\n")

	for _, u := range s.List() {
		if u.NTHash == "" {
			continue
		}
		flags := "U          "
		if !u.Enabled {
			flags = "DU         "
		}
		lct := u.PasswordSetAt.Unix()
		if lct < 0 {
			lct = 0
		}
		fmt.Fprintf(&sb, "%s:%d:%s:%s:[%s]:LCT-%X:\n",
			u.UID, u.UIDNumber, noLMHash, strings.ToUpper(u.NTHash), flags, lct)
	}
	return sb.String()
}
