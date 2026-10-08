//! Sanitises values that originate from a request or a peer before they are
//! written to a log line (port of `api/internal/logsafe`, gosec G706/AUDIT-L5).
//! Without it a crafted dataset, pool, pod or user name could inject newlines
//! (forging whole log entries) or terminal escapes into the operator's logs.

/// Caps a sanitised field so one hostile name cannot flood the log.
const MAX_FIELD_LEN: usize = 200;

/// Returns `s` with control characters replaced by `.` and its length capped.
/// Printable characters are left untouched, so normal names read exactly as
/// before.
pub fn field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (written, ch) in s.chars().enumerate() {
        if written >= MAX_FIELD_LEN {
            out.push_str("...");
            break;
        }
        if (ch as u32) < 0x20 || ch == '\u{7f}' {
            out.push('.');
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::field;

    #[test]
    fn passes_normal_names_through() {
        assert_eq!(field("tank/media"), "tank/media");
    }

    #[test]
    fn replaces_control_characters() {
        assert_eq!(field("a\nb\tc"), "a.b.c");
        assert_eq!(field("evil\x7f"), "evil.");
    }

    #[test]
    fn caps_length_with_ellipsis() {
        let long = "x".repeat(500);
        let out = field(&long);
        assert_eq!(out, format!("{}...", "x".repeat(200)));
    }
}
