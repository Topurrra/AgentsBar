//! Redaction for the log sink.
//!
//! Wired into the `tauri_plugin_log` format callback in [`crate::run`], so redaction is a
//! property of the sink rather than of every author. A single careless
//! `log::error!("{body}")` in a future provider cannot leak a credential through it.
//!
//! Ported from CodexBar's `Sources/CodexBarCore/Logging/LogRedactor.swift`, including its
//! cheap substring prefilter: a normal log line never reaches the scanning path.
//!
//! ponytail: hand written matching instead of the `regex` crate. Five fixed shapes do not
//! justify a new dependency (and 1.5 MB of binary) in a 4.6 MB app.

use std::borrow::Cow;

const PLACEHOLDER: &str = "<redacted>";
/// Header names whose whole value is a secret, so everything after the colon goes.
const SECRET_HEADERS: [&str; 2] = ["cookie", "authorization"];
/// Punctuation a token is commonly wrapped in inside a log line: `"sk-live-x"`, `(sk-x)`.
const EDGE: &[char] = &[
    '"', '\'', '`', '(', ')', '[', ']', '{', '}', '<', '>', ',', ';', '.',
];

/// Redact a log message. Returns the input untouched when nothing looks sensitive.
pub fn redact(text: &str) -> Cow<'_, str> {
    if !may_contain_secret(text) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        redact_line(line, &mut out);
        if line.len() != raw.len() {
            out.push('\n');
        }
    }
    Cow::Owned(out)
}

/// The cheap gate: no allocation, one pass, and false only when nothing can match.
fn may_contain_secret(text: &str) -> bool {
    text.contains('@')
        || ["sk-", "gh", "bearer", "cookie", "authorization"]
            .iter()
            .any(|needle| contains_ignore_case(text, needle))
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    h.len() >= n.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

fn redact_line(line: &str, out: &mut String) {
    // A `cookie:` or `authorization:` line: everything after the colon is the secret.
    if let Some(value_at) = secret_header_value_start(line) {
        out.push_str(&line[..value_at]);
        out.push_str(PLACEHOLDER);
        return;
    }
    let mut after_bearer = false;
    for chunk in line.split_inclusive(char::is_whitespace) {
        let word = chunk.trim_end();
        if word.is_empty() {
            out.push_str(chunk);
            continue;
        }
        if after_bearer {
            out.push_str(PLACEHOLDER);
        } else {
            push_word(word, out);
        }
        // `bearer` catches `bearer <token>` even when no other rule would.
        after_bearer = word.eq_ignore_ascii_case("bearer");
        out.push_str(&chunk[word.len()..]);
    }
}

/// Byte index just past `cookie:` / `authorization:` and its trailing spaces, if the line
/// carries one. `cookies.rs` or `3 cookie(s)` do not match: the colon must follow the name.
fn secret_header_value_start(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for name in SECRET_HEADERS {
        let n = name.len();
        for start in 0..bytes.len().saturating_sub(n) {
            if !bytes[start..start + n].eq_ignore_ascii_case(name.as_bytes()) {
                continue;
            }
            let mut i = start + n;
            while bytes.get(i) == Some(&b' ') {
                i += 1;
            }
            if bytes.get(i) != Some(&b':') {
                continue;
            }
            i += 1;
            while bytes.get(i) == Some(&b' ') {
                i += 1;
            }
            return Some(i);
        }
    }
    None
}

/// Emit one word, replacing its core with the placeholder when it is secret shaped and
/// keeping whatever punctuation wrapped it.
fn push_word(word: &str, out: &mut String) {
    let core = word.trim_start_matches(EDGE);
    let lead = word.len() - core.len();
    let core = core.trim_end_matches(EDGE);
    // In a `name=value` pair only the VALUE is tested, so `token=sk-...` keeps the name
    // and loses the value. The name itself buys nothing: `session=abc123` does not match
    // any shape below and passes through, which is why a whole `cookie:` or
    // `authorization:` header line is cut above rather than word by word here.
    let value = match core.split_once('=') {
        Some((_, v)) if !v.is_empty() => v,
        _ => core,
    };
    if value.is_empty() || !is_secret(value) {
        out.push_str(word);
        return;
    }
    out.push_str(&word[..lead]);
    out.push_str(&core[..core.len() - value.len()]);
    out.push_str(if value.contains('@') {
        "<redacted-email>"
    } else {
        PLACEHOLDER
    });
    out.push_str(&word[lead + core.len()..]);
}

fn is_secret(core: &str) -> bool {
    let b = core.as_bytes();
    // `sk-...`: OpenAI, Anthropic, MiniMax, DeepSeek and most of the rest.
    if b.len() > 3 && b[..3].eq_ignore_ascii_case(b"sk-") {
        return true;
    }
    // GitHub: ghp_, gho_, ghu_, ghs_, ghr_.
    if b.len() > 4
        && b[..2].eq_ignore_ascii_case(b"gh")
        && b[3] == b'_'
        && b"pousr".contains(&b[2].to_ascii_lowercase())
    {
        return true;
    }
    is_email(core)
}

/// Deliberately loose: `a@b.c`. A log line is not a signup form, and over-redacting a
/// string that merely looks like an address costs nothing.
fn is_email(core: &str) -> bool {
    let Some((local, domain)) = core.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.contains('@')
        && domain
            .split_once('.')
            .is_some_and(|(host, tld)| !host.is_empty() && tld.len() >= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value here is synthetic. None of it is, or ever was, a real credential.
    #[test]
    fn fake_credentials_do_not_survive_the_sink() {
        let cases = [
            (
                "openai refresh failed for sk-proj-FAKE0000TOKEN",
                "openai refresh failed for <redacted>",
            ),
            ("token=ghp_FAKE000000000000", "token=<redacted>"),
            ("ghp_FAKE000000000000 rejected", "<redacted> rejected"),
            ("(sk-ant-FAKE000).", "(<redacted>)."),
            (
                "Authorization: Bearer sk-FAKE000",
                "Authorization: <redacted>",
            ),
            ("cookie: session=FAKE; other=FAKE", "cookie: <redacted>"),
            (
                "sent bearer FAKE0TOKEN to host",
                "sent bearer <redacted> to host",
            ),
            (
                "signed in as someone@example.com",
                "signed in as <redacted-email>",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "input: {input}");
        }
    }

    /// Ordinary lines come out byte identical, including the ones that merely mention a
    /// cookie: `cookies.rs` and `3 cookie(s)` are not `cookie:` headers.
    #[test]
    fn ordinary_lines_are_untouched() {
        for line in [
            "cursor: using 3 cookie(s) from Chrome",
            "removed 2 leftover cookie database cop(ies) from the temp folder",
            "codex: 5h window 41% used, resets in 2h",
            "history save failed: permission denied",
        ] {
            assert_eq!(redact(line), line);
        }
        // And a line with no trigger word at all never leaves the prefilter.
        let plain = "codex: 5h window 41% used, resets in 2h";
        assert!(matches!(redact(plain), Cow::Borrowed(_)));
    }

    #[test]
    fn multiline_keeps_its_shape() {
        let text = "GET /usage\ncookie: a=b\nstatus 200\n";
        assert_eq!(redact(text), "GET /usage\ncookie: <redacted>\nstatus 200\n");
    }
}
