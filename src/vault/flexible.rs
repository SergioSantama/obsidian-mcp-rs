//! Whitespace-tolerant fallback matching for `find_and_replace`.
//!
//! The byte-exact `contains` path is the contract, and it stays first. This
//! module only fires when that path misses — which in the field means the
//! needle the model SENT is not the needle it MEANT: the transport between
//! model and server has been observed mangling tool-input text (BrainVault
//! #314 — a two-line `search` byte-identical to the note still missed; #160 /
//! #187 — enum params arriving unquoted, calls arriving wrapped in
//! `__unparsedToolInput`). The two manglings that produce a *valid but wrong*
//! needle are a line break that arrived as some other whitespace (space), and
//! `\n` that arrived as the literal two characters backslash-n.
//!
//! So the fallback matches a needle where every whitespace RUN may correspond
//! to any whitespace RUN in the note (`a\nb` still finds `a b` and `a\r\nb`),
//! and also tries the needle with `\n`/`\r`/`\t` escape pairs unescaped first.
//! A tolerant match edits nothing unless it is UNIQUE — an ambiguous match is
//! a guess, and guessing is how notes get silently damaged.

use std::ops::Range;

/// One fallback candidate for `needle`, most faithful first:
/// (1) the needle with literal `\n`/`\r`/`\t` two-character escape sequences
///     unescaped — the double-escaped emission case;
/// (2) the needle as sent.
/// Each is then matched whitespace-flexibly. The exact-as-sent candidate is
/// deliberately absent — the caller only gets here after the exact match
/// already failed.
fn candidates(needle: &str) -> Vec<String> {
    if needle.contains("\\n") || needle.contains("\\r") || needle.contains("\\t") {
        // The unescaped twin: `\r\n` first so a Windows escape pair does not
        // become two separate `\r`→`\n` rewrites.
        let unescaped = needle
            .replace("\\r\\n", "\n")
            .replace("\\n", "\n")
            .replace("\\r", "\n")
            .replace("\\t", "\t");
        Vec::from([unescaped, needle.to_string()])
    } else {
        Vec::from([needle.to_string()])
    }
}

/// Build a regex from `needle` where each literal chunk (split on whitespace
/// runs) is escaped and the runs become `\s+`. Returns `None` for a needle
/// that is empty or only whitespace — there is nothing meaningful to match.
fn flexible_pattern(needle: &str) -> Option<regex::Regex> {
    let trimmed = needle.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut pattern = String::with_capacity(needle.len() + 16);
    let mut parts = needle.split_whitespace().map(regex::escape).peekable();
    while let Some(part) = parts.next() {
        pattern.push_str(&part);
        if parts.peek().is_some() {
            pattern.push_str(r"\s+");
        }
    }
    regex::Regex::new(&pattern).ok()
}

/// Find the byte span of `needle` in `text`, tolerating whitespace-run
/// differences and literal escape pairs. `Some` only when exactly one span
/// matches; `None` otherwise (zero matches, or an ambiguous one).
pub(crate) fn flexible_find(text: &str, needle: &str) -> Option<Range<usize>> {
    if needle.trim().is_empty() {
        return None;
    }
    for candidate in candidates(needle) {
        let Some(pattern) = flexible_pattern(&candidate) else {
            continue;
        };
        let mut spans = pattern.find_iter(text).map(|m| m.range());
        if let (Some(only), None) = (spans.next(), spans.next()) {
            return Some(only);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_of(text: &str, needle: &str) -> Option<String> {
        flexible_find(text, needle).map(|r| text[r].to_string())
    }

    #[test]
    fn exact_multiline_matches_byte_for_byte() {
        let text = "linea-A-uno\nlinea-A-dos\n";
        assert_eq!(
            span_of(text, "linea-A-uno\nlinea-A-dos").as_deref(),
            Some("linea-A-uno\nlinea-A-dos")
        );
    }

    #[test]
    fn needle_newline_finds_note_space() {
        // The #314 shape: the sent needle's line break arrived as a space.
        let text = "linea-A-uno linea-A-dos\n";
        assert_eq!(
            span_of(text, "linea-A-uno linea-A-dos").as_deref(),
            Some("linea-A-uno linea-A-dos")
        );
        // …and the reverse direction: needle space, note line break.
        assert_eq!(span_of("a\nb\n", "a b").as_deref(), Some("a\nb"));
    }

    #[test]
    fn needle_newline_finds_note_crlf() {
        assert_eq!(span_of("a\r\nb\n", "a\nb").as_deref(), Some("a\r\nb"));
    }

    #[test]
    fn literal_backslash_n_escape_is_tried_unescaped() {
        // A double-escaped emission: the needle carries the two characters
        // backslash + n where the note has a real line break.
        let text = "linea-A-uno\nlinea-A-dos\n";
        let sent = "linea-A-uno\\nlinea-A-dos";
        assert_eq!(
            span_of(text, sent).as_deref(),
            Some("linea-A-uno\nlinea-A-dos")
        );
    }

    #[test]
    fn ambiguous_flexible_match_is_refused() {
        // Two whitespace-tolerant candidates: edit nothing rather than guess.
        let text = "a b\na\nb\n";
        assert_eq!(span_of(text, "a\nb"), None);
    }

    #[test]
    fn no_match_at_all_is_none() {
        assert_eq!(span_of("completely different\n", "a\nb"), None);
    }

    #[test]
    fn whitespace_only_needle_is_refused() {
        assert_eq!(span_of("a\nb\n", "  \n "), None);
    }

    #[test]
    fn regex_metacharacters_in_needle_are_literal() {
        // `.` must not stand in for an arbitrary character: `a b` must match
        // the whitespace run between a and b, never the dot in `a.b`.
        let text = "a.b a\nb\n";
        assert_eq!(span_of(text, "a b").as_deref(), Some("a\nb"));
    }

    #[test]
    fn leading_and_trailing_whitespace_in_needle_is_forgiven() {
        // A needle that picked up a trailing line break still lands.
        assert_eq!(
            span_of("uno\ndos\n", "uno\ndos\n").as_deref(),
            Some("uno\ndos")
        );
    }
}
