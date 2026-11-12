//! Lenient recovery of tool input that arrives wrapped in the
//! `__unparsedToolInput` wrapper.
//!
//! When the model's JSON for a tool call fails to parse upstream of the
//! server (an enum value emitted without quotes, a literal line break inside
//! a string), the call can reach the server as
//! `{"__unparsedToolInput": "<raw text>"}` instead of real arguments.
//! Strictly, the server can only reject it — every tool requires fields the
//! wrapper doesn't carry. That is the deterministic enum failure documented
//! in the Brain project (brain-obsidian PENDIENTES 10-11; BrainVault #160,
//! #187, #49): calls whose only sin is an unquoted `view`/`searchType`/
//! `targetType` die with an invalid-params error, while identical calls with
//! quoted enums succeed.
//!
//! The raw text is almost always nearly-valid JSON, so `recover` re-parses
//! it leniently and, when the result is a JSON object, replaces the
//! arguments with the recovered values — the tool then runs exactly as if
//! the model had emitted valid JSON. On failure the arguments keep whatever
//! else they carried and the usual validation error surfaces, so nothing
//! silently changes meaning.

use serde_json::{Map, Value};

/// The key under which the raw, unparsed tool input arrives.
pub const UNPARSED_KEY: &str = "__unparsedToolInput";

/// If `arguments` carries `__unparsedToolInput`, parse the raw text leniently
/// and replace the arguments with the recovered object. `false` = nothing
/// recovered (no wrapper present, or the raw text did not parse into a JSON
/// object); the caller's normal validation then applies.
pub fn recover(arguments: &mut Map<String, Value>) -> bool {
    // Only a plain string wrapper is the shape this module repairs. Anything
    // else (object, number, absent) is restored and left for normal
    // validation to reject with its usual, honest error.
    let raw = match arguments.remove(UNPARSED_KEY) {
        Some(Value::String(raw)) => raw,
        Some(other) => {
            arguments.insert(UNPARSED_KEY.into(), other);
            return false;
        }
        None => return false,
    };
    match parse_lenient(&raw) {
        // The raw text IS the intended arguments object — extend the map with
        // whatever the raw text carries. Anything else (array, scalar,
        // unparseable) recovers nothing.
        Some(obj) if obj.is_object() => {
            arguments.extend(
                obj.as_object()
                    .expect("just checked is_object")
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            true
        }
        _ => false,
    }
}

/// Parse JSON tolerantly: accepts what a model actually emits when its strict
/// JSON comes out malformed — unquoted object keys, unquoted string values
/// (the enum case), and literal control characters (line breaks) inside
/// strings. Returns `None` when the text still does not parse.
///
/// Two sanitizing passes over the raw text, then a strict parse. Sanitizing
/// keeps the strict parser as the single authority: the passes can only make
/// strict-valid text more valid, and text that was already strict passes
/// through byte-identical.
pub fn parse_lenient(raw: &str) -> Option<Value> {
    let escaped = escape_bare_control_chars(raw);
    let quoted = quote_bare_identifiers(&escaped);
    serde_json::from_str(&quoted).ok()
}

/// Rewrite every literal control character (< 0x20) that sits INSIDE a string
/// literal as its escaped form. Outside strings, control characters are
/// whitespace the strict parser accepts.
fn escape_bare_control_chars(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    let mut in_string = false;
    let mut escaped_next = false;
    for c in raw.chars() {
        if in_string && !escaped_next {
            match c {
                '\\' => escaped_next = true,
                '"' => in_string = false,
                '\n' => {
                    out.push_str("\\n");
                    continue;
                }
                '\r' => {
                    out.push_str("\\r");
                    continue;
                }
                '\t' => {
                    out.push_str("\\t");
                    continue;
                }
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                    continue;
                }
                _ => {}
            }
        } else if in_string {
            // The character after a backslash: emitted untouched.
            escaped_next = false;
        } else if c == '"' {
            in_string = true;
        }
        out.push(c);
    }
    out
}

/// Whether `c` can appear inside an unquoted word this pass will quote.
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')
}

/// Quote bare identifiers in non-string positions — but only where a JSON
/// key or value can legally start, i.e. right after `:` `{` `,` or `[`. A
/// word there is an unquoted KEY when followed (past whitespace) by `:`, and
/// an unquoted VALUE otherwise (`true`/`false`/`null` stay literal). A word
/// anywhere else (the `e` in `-1.5e3`, prose) is left alone. `:` ends a word
/// (so `view:outline` splits into key `view` + value `outline`), which means
/// unquoted values containing `:` (Windows paths) do not repair — they fail
/// the strict parse and recover nothing, the safe direction.
fn quote_bare_identifiers(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 16);
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped_next = false;
    while i < n {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped_next {
                escaped_next = false;
            } else {
                match c {
                    '\\' => escaped_next = true,
                    '"' => in_string = false,
                    _ => {}
                }
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            // Quoteable only where a key or a value can legally start.
            let quoteable = out
                .chars()
                .rev()
                .find(|ch| !ch.is_whitespace())
                .is_some_and(|prev| matches!(prev, ':' | '{' | ',' | '['));
            // Read the full word.
            let start = i;
            while i < n && is_word_char(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if !quoteable {
                out.push_str(&word);
                continue;
            }
            // Peek past whitespace for the colon that marks a key.
            let mut j = i;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            let is_key = j < n && chars[j] == ':';
            if is_key || !matches!(word.as_str(), "true" | "false" | "null") {
                out.push('"');
                out.push_str(&word);
                out.push('"');
            } else {
                out.push_str(&word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_json_passes_through_untouched() {
        let raw = r#"{"vault":"Brain","filename":"x.md","view":"outline"}"#;
        let parsed = parse_lenient(raw).expect("strict JSON must parse");
        assert_eq!(parsed["view"], "outline");
        assert_eq!(parsed["vault"], "Brain");
    }

    #[test]
    fn unquoted_enum_value_recovers() {
        let raw = r#"{"vault":"Brain","filename":"x.md","view":outline}"#;
        let parsed = parse_lenient(raw).expect("unquoted enum value must repair");
        assert_eq!(parsed["view"], "outline");
    }

    #[test]
    fn unquoted_key_recovers() {
        let raw = r#"{vault:"Brain",filename:"x.md"}"#;
        let parsed = parse_lenient(raw).expect("unquoted key must repair");
        assert_eq!(parsed["vault"], "Brain");
        assert_eq!(parsed["filename"], "x.md");
    }

    #[test]
    fn literal_newline_inside_string_recovers() {
        // The raw bytes the model emitted: a REAL line break inside the
        // search string — invalid JSON, valid intent.
        let raw = "{\"vault\":\"Brain\",\"filename\":\"x.md\",\"search\":\"uno\ndos\"}";
        let parsed = parse_lenient(raw).expect("literal newline must be escaped");
        assert_eq!(parsed["search"], "uno\ndos");
    }

    #[test]
    fn literal_newline_and_unquoted_enum_together() {
        let raw = "{vault:Brain,search:\"uno\ndos\",view:outline}";
        let parsed = parse_lenient(raw).expect("combined malformations must repair");
        assert_eq!(parsed["vault"], "Brain");
        assert_eq!(parsed["search"], "uno\ndos");
        assert_eq!(parsed["view"], "outline");
    }

    #[test]
    fn true_false_null_stay_literal() {
        let raw = r#"{"a":true,"b":false,"c":null,"d":maybe}"#;
        let parsed = parse_lenient(raw).expect("must parse");
        assert_eq!(parsed["a"], true);
        assert_eq!(parsed["b"], false);
        assert_eq!(parsed["c"], Value::Null);
        assert_eq!(parsed["d"], "maybe");
    }

    #[test]
    fn numbers_and_nested_structures_survive() {
        let raw = r#"{"n":-1.5e3,"list":[1,"two",{"k":v}],"u":x_y-z.md}"#;
        let parsed = parse_lenient(raw).expect("must parse");
        assert_eq!(parsed["n"], -1500.0);
        assert_eq!(parsed["list"][1], "two");
        assert_eq!(parsed["list"][2]["k"], "v");
        assert_eq!(parsed["u"], "x_y-z.md");
    }

    #[test]
    fn garbage_still_fails() {
        assert_eq!(parse_lenient("not json at all"), None);
        assert_eq!(parse_lenient("{\"a\":}"), None);
        assert_eq!(parse_lenient("[1,2"), None);
    }

    #[test]
    fn recover_replaces_arguments() {
        let mut args =
            json!({"__unparsedToolInput": "{vault:Brain,filename:\"n.md\",view:outline}"})
                .as_object()
                .cloned()
                .unwrap();
        assert!(recover(&mut args));
        assert_eq!(args["vault"], "Brain");
        assert_eq!(args["view"], "outline");
        assert!(
            !args.contains_key(UNPARSED_KEY),
            "the wrapper must not survive"
        );
    }

    #[test]
    fn recover_finds_multiline_search_in_wrapper() {
        // The #314 family: the needle arrives inside the wrapper with its
        // literal line break intact — the exact form the fallback matcher in
        // `vault::flexible` was built for.
        let mut args = json!({
            "__unparsedToolInput": "{vault:Brain,filename:\"n.md\",operation:find_and_replace,search:\"uno\ndos\",content:\"x\"}"
        })
        .as_object()
        .cloned()
        .unwrap();
        assert!(recover(&mut args));
        assert_eq!(args["search"], "uno\ndos");
        assert_eq!(args["operation"], "find_and_replace");
    }

    #[test]
    fn unrecoverable_wrapper_leaves_other_args_alone() {
        let mut args = json!({"__unparsedToolInput": "utter garbage [[(", "keep": "me"})
            .as_object()
            .cloned()
            .unwrap();
        assert!(!recover(&mut args));
        assert_eq!(args["keep"], "me");
        assert!(!args.contains_key(UNPARSED_KEY));
    }

    #[test]
    fn no_wrapper_is_a_no_op() {
        let mut args = json!({"vault": "Brain"}).as_object().cloned().unwrap();
        assert!(!recover(&mut args));
        assert_eq!(args["vault"], "Brain");
    }

    #[test]
    fn non_string_wrapper_is_preserved_untouched() {
        let mut args = json!({"__unparsedToolInput": 42})
            .as_object()
            .cloned()
            .unwrap();
        assert!(!recover(&mut args));
        assert_eq!(args["__unparsedToolInput"], 42);
    }
}
