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
	// GIDNumber is the POSIX primary group id from the LDAP entry.
	GIDNumber int `json:"gidNumber"`
	// Gecos is the display name used for the NSS passwd entry.
	Gecos string `json:"gecos"`
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

// PosixIdentity describes the POSIX account attributes mirrored to the node.
// Samba maps an SMB session to a UNIX uid, so the serving container must be
// able to resolve the user through NSS; these values are what make that work
// without any local account being created on the node.
type PosixIdentity struct {
	UID     string
	UIDNum  int
	GIDNum  int
	Gecos   string
	HomeDir string
	Shell   string
}

// Upsert records the account's NT hash and POSIX identity. A zero uidNumber or
// gidNumber leaves any previously known value untouched.
func (s *SambaUserStore) Upsert(id PosixIdentity, ntHash string) error {
	uid := strings.TrimSpace(id.UID)
	if uid == "" {
		return fmt.Errorf("uid is required")
	}
	if ntHash == "" {
		return fmt.Errorf("NT hash is required for %s", uid)
	}
	if id.HomeDir == "" {
		id.HomeDir = "/home/" + uid
	}
	if id.Shell == "" {
		id.Shell = "/bin/bash"
	}
	if id.Gecos == "" {
		id.Gecos = uid
	}

	if existing, ok := s.users[uid]; ok {
		existing.NTHash = strings.ToUpper(ntHash)
		existing.PasswordSetAt = time.Now().UTC()
		if id.UIDNum > 0 {
			existing.UIDNumber = id.UIDNum
		}
		if id.GIDNum > 0 {
			existing.GIDNumber = id.GIDNum
		}
		if id.Gecos != "" {
			existing.Gecos = id.Gecos
		}
	} else {
		s.users[uid] = &SambaUser{
			UID:           uid,
			UIDNumber:     id.UIDNum,
			GIDNumber:     id.GIDNum,
			Gecos:         id.Gecos,
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

// RenderPasswd renders /var/lib/extrausers/passwd entries. The naslos-samba
// container is configured with `passwd: files extrausers`, which lets Samba
// resolve LDAP users (and therefore map an SMB session to a UNIX uid) without
// creating local accounts on the node and without LDAP credentials in the
// serving container: the API already holds the uid/gid for each user.
func (s *SambaUserStore) RenderPasswd() string {
	var sb strings.Builder
	sb.WriteString("# Generated by Naslos - do not edit.\n")
	for _, u := range s.List() {
		if u.UIDNumber <= 0 {
			// An entry without a POSIX uid cannot be resolved, and a bogus
			// one would make Samba store 4294967295 and deny every login.
			continue
		}
		gid := u.GIDNumber
		if gid <= 0 {
			gid = 10000
		}
		gecos := u.Gecos
		if gecos == "" {
			gecos = u.UID
		}
		fmt.Fprintf(&sb, "%s:x:%d:%d:%s:/home/%s:/bin/bash\n",
			u.UID, u.UIDNumber, gid, gecos, u.UID)
	}
	return sb.String()
}

// RenderGroup renders /var/lib/extrausers/group entries for the primary groups
// referenced by the mirrored accounts, so `getgrgid` resolves inside the
// serving container as well.
func (s *SambaUserStore) RenderGroup() string {
	// Primary gid -> member uids, preserving a stable order.
	gids := make(map[int][]string)
	for _, u := range s.List() {
		if u.UIDNumber <= 0 {
			continue
		}
		gid := u.GIDNumber
		if gid <= 0 {
			gid = 10000
		}
		gids[gid] = append(gids[gid], u.UID)
	}

	ordered := make([]int, 0, len(gids))
	for gid := range gids {
		ordered = append(ordered, gid)
	}
	sort.Ints(ordered)

	var sb strings.Builder
	sb.WriteString("# Generated by Naslos - do not edit.\n")
	for _, gid := range ordered {
		members := gids[gid]
		sort.Strings(members)
		// Name the well-known Naslos groups; anything else is group<gid>.
		name := fmt.Sprintf("group%d", gid)
		if gid == 10000 {
			name = "naslos_users"
		}
		fmt.Fprintf(&sb, "%s:x:%d:%s\n", name, gid, strings.Join(members, ","))
	}
	return sb.String()
}

// RenderShadow renders /var/lib/extrausers/shadow entries. The field is kept
// "*" (no UNIX password): authentication is done by Samba's passdb, and an
// empty/absent shadow entry is what makes the account look expired on some
// systems. Explicit far-future expiry avoids that.
func (s *SambaUserStore) RenderShadow() string {
	var sb strings.Builder
	sb.WriteString("# Generated by Naslos - do not edit.\n")
	for _, u := range s.List() {
		if u.UIDNumber <= 0 {
			continue
		}
		// uid:*:lastchange:min:max:warn:inactive:expire:reserved
		fmt.Fprintf(&sb, "%s:*:19000:0:99999:7:::\n", u.UID)
	}
	return sb.String()
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
