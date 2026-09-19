// Package logsafe sanitises values that originate from a request or a peer
// before they are written to a log line. Without it a crafted dataset, pool,
// pod or user name can inject newlines (forging whole log entries) or terminal
// escapes into the operator's logs - gosec G706, AUDIT-L5.
package logsafe

import "strings"

// maxFieldLen caps a sanitised field so one hostile name cannot flood the log.
const maxFieldLen = 200

// Field returns s with control characters replaced by '.' and its length capped.
// Printable characters are left untouched, so normal names read exactly as
// before.
func Field(s string) string {
	var b strings.Builder
	b.Grow(len(s))

	written := 0
	for _, r := range s {
		if written >= maxFieldLen {
			b.WriteString("...")
			break
		}
		if r < 0x20 || r == 0x7f {
			b.WriteRune('.')
		} else {
			b.WriteRune(r)
		}
		written++
	}
	return b.String()
}
