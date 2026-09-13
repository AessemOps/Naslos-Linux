package identity

import "testing"

// TestComputeNTHashKnownVector pins the NT hash algorithm (MD4 of the
// UTF-16LE password). The vectors are the values Samba/Windows produce for
// these passwords, computed independently with
// `printf '%s' <pw> | iconv -t UTF-16LE | openssl dgst -md4`.
// Getting this wrong would silently break every SMB login, so it is asserted
// rather than assumed.
func TestComputeNTHashKnownVector(t *testing.T) {
	cases := []struct {
		password string
		want     string
	}{
		// "NaslosTest123" — verified against OpenSSL MD4.
		{"NaslosTest123", "6574CC330574C3FB138E544592D125C9"},
		// The classic published NT hash vector.
		{"password", "8846F7EAEE8FB117AD06BDD830B7586C"},
		{"", "31D6CFE0D16AE931B73C59D7E0C089C0"},
	}

	for _, tc := range cases {
		if got := computeNTHash(tc.password); got != tc.want {
			t.Errorf("computeNTHash(%q) = %s, want %s", tc.password, got, tc.want)
		}
	}
}

// TestComputeNTHashIsUppercaseHex guards the output format expected by
// `pdbedit -i smbpasswd:`.
func TestComputeNTHashIsUppercaseHex(t *testing.T) {
	got := computeNTHash("NaslosTest123")
	if len(got) != 32 {
		t.Fatalf("NT hash must be 32 hex chars, got %d (%q)", len(got), got)
	}
	for _, c := range got {
		if (c < '0' || c > '9') && (c < 'A' || c > 'F') {
			t.Fatalf("NT hash contains a non-uppercase-hex character: %q", got)
		}
	}
}
