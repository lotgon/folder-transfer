//! JSONC config: comment stripping (a port of `Remove-JsonComments`) plus the
//! serde models for the serve config and the generated-client connection file.
//! STRICT: malformed JSON (a lone backslash, a trailing comma) is an error, not
//! auto-fixed (matches the PowerShell stance). See spec sections 7.4 and 7.5.

use serde::{Deserialize, Serialize};

use crate::BoxError;

/// Strip `//` line and `/* */` block comments, string-aware (content inside
/// `"..."`, honouring `\"`, is left untouched). Exact port of `Remove-JsonComments`.
pub fn strip_jsonc(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    let mut esc = false;
    while i < n {
        let c = chars[i];
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Serve-side config file. Unknown keys are ignored (like `ConvertFrom-Json`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServeFileConfig {
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    pub port: Option<u16>,
    pub allow_ip: Option<String>,
    pub server_host: Option<String>,
    pub idle_seconds: Option<u64>,
    pub stall_timeout: Option<u64>,
    pub client_out: Option<String>,
    pub cutover: Option<bool>,
    pub once: Option<bool>,
    pub no_firewall: Option<bool>,
    pub streams: Option<i32>,
    pub compress: Option<bool>,
    /// Adaptive-level coefficient: keep compression >= this x the link speed (default 1.6).
    pub compress_margin: Option<f64>,
}

impl ServeFileConfig {
    pub fn load(path: &str) -> Result<Self, BoxError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("config file not found / unreadable: {path}: {e}"))?;
        let stripped = strip_jsonc(&raw);
        let cfg: Self = serde_json::from_str(&stripped).map_err(|e| -> BoxError {
            format!(
                "invalid JSON in {path}: {e}\n  Tip: use forward slashes \"C:/path\" or DOUBLED \
                 backslashes \"C:\\\\path\", and no trailing commas. ( // and /* */ comments are allowed. )"
            )
            .into()
        })?;
        // Best-effort: bring an older config file up to the current schema (adds any
        // missing tunables with their defaults so they're visible and editable). Never
        // fatal — a read-only / managed config just stays as-is.
        if let Err(e) = upgrade_serve_file(path, &raw, &stripped) {
            eprintln!("[ft] note: could not auto-upgrade config {path}: {e}");
        }
        Ok(cfg)
    }
}

/// Current serve-config tunables: key, default JSON value, one-line doc. Used to
/// fill in an older config file. Deliberately EXCLUDES the content keys
/// (folder/folders/ignore) — those are the user's data, not defaults to inject.
const SERVE_UPGRADE_KEYS: &[(&str, &str, &str)] = &[
    ("port", "8722", "TCP port to listen on"),
    ("streams", "4", "parallel streams (1 = classic SYNC, >1 = parallel)"),
    ("compress", "true", "adaptive compression on/off"),
    ("compressMargin", "1.6", "keep compression at least this x the link speed"),
    ("idleSeconds", "600", "auto-stop after this many idle seconds with no client"),
    ("stallTimeout", "300", "abort a connected-but-silent client after this many seconds"),
    ("once", "false", "exit after one clean transfer"),
    ("cutover", "false", "two-phase cutover (implies once, streams = 1)"),
    ("noFirewall", "false", "do not touch the Windows firewall (no-op on Linux)"),
    ("allowIp", "\"\"", "restrict to one source IP (empty = any)"),
    ("serverHost", "\"\"", "address put in the generated client (empty = auto-detect)"),
    ("clientOut", "\"\"", "where to write the generated client file (empty = default)"),
];

/// Append any missing current keys (with defaults + a comment) to an older serve
/// config, preserving the file's existing content and comments. Saves the original
/// to `<path>.bak` first. Validates the result and only writes if it still parses,
/// so a parsing edge case can never corrupt the live config.
fn upgrade_serve_file(path: &str, raw: &str, stripped: &str) -> Result<(), BoxError> {
    let val: serde_json::Value = serde_json::from_str(stripped)?;
    let obj = match val.as_object() {
        Some(o) => o,
        None => return Ok(()), // not an object literal -> leave it alone
    };
    let missing: Vec<&(&str, &str, &str)> =
        SERVE_UPGRADE_KEYS.iter().filter(|(k, _, _)| !obj.contains_key(*k)).collect();
    if missing.is_empty() {
        return Ok(()); // already current
    }

    // Insert right after the last non-whitespace char before the root's closing '}'.
    let close = raw.rfind('}').ok_or("config has no closing brace")?;
    let bytes = raw.as_bytes();
    let mut at = close;
    while at > 0 && bytes[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    let has_existing_keys = at > 0 && bytes[at - 1] != b'{';
    let mut block = String::new();
    if has_existing_keys {
        block.push(','); // close off the previous key/value
    }
    block.push_str(&format!(
        "\n\n  // ---- added by ft {} (current defaults; edit as needed) ----",
        env!("CARGO_PKG_VERSION")
    ));
    for (i, (k, v, c)) in missing.iter().enumerate() {
        let comma = if i + 1 < missing.len() { "," } else { "" };
        block.push_str(&format!("\n  \"{k}\": {v}{comma} // {c}"));
    }
    let result = format!("{}{}{}", &raw[..at], block, &raw[at..]);

    // Re-parse the result before touching disk — never write something that won't load.
    serde_json::from_str::<ServeFileConfig>(&strip_jsonc(&result))
        .map_err(|e| format!("upgraded config would be invalid, not written: {e}"))?;

    std::fs::write(format!("{path}.bak"), raw.as_bytes())?;
    std::fs::write(path, result.as_bytes())?;
    let added: Vec<&str> = missing.iter().map(|(k, _, _)| *k).collect();
    eprintln!(
        "[ft] config {path}: added {} new field(s) ({}); original saved to {path}.bak",
        added.len(),
        added.join(", ")
    );
    Ok(())
}

/// Client connection file written by the server and read by `ft get --config`.
/// Only the essentials are written (server/port/token/fingerprint); `ignore` and
/// `streams` are sent by the server after connecting, so they are optional here
/// (read as overrides if an older file still carries them).
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientConn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streams: Option<i32>,
}

impl ClientConn {
    pub fn load(path: &str) -> Result<Self, BoxError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("config file not found / unreadable: {path}: {e}"))?;
        let stripped = strip_jsonc(&raw);
        serde_json::from_str(&stripped).map_err(|e| format!("invalid JSON in {path}: {e}").into())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_and_block_comments_outside_strings() {
        let s = r#"{
            // a line comment
            "folder": "C:/data", /* block */ "port": 8722,
            "url": "http://x/y" // keep the // inside the string
        }"#;
        let out = strip_jsonc(s);
        assert!(!out.contains("a line comment"));
        assert!(!out.contains("/* block */"));
        assert!(out.contains("http://x/y"), "// inside a string must be preserved");
        let cfg: ServeFileConfig = serde_json::from_str(&out).unwrap();
        assert_eq!(cfg.folder.as_deref(), Some("C:/data"));
        assert_eq!(cfg.port, Some(8722));
    }

    #[test]
    fn trailing_comma_is_an_error() {
        let s = strip_jsonc(r#"{ "port": 8722, }"#);
        assert!(serde_json::from_str::<ServeFileConfig>(&s).is_err());
    }

    #[test]
    fn camelcase_keys_map() {
        let s = strip_jsonc(r#"{ "allowIp": "10.0.0.5", "idleSeconds": 30, "noFirewall": true, "compress": false }"#);
        let cfg: ServeFileConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(cfg.allow_ip.as_deref(), Some("10.0.0.5"));
        assert_eq!(cfg.idle_seconds, Some(30));
        assert_eq!(cfg.no_firewall, Some(true));
        assert_eq!(cfg.compress, Some(false));
    }

    #[test]
    fn unknown_keys_ignored() {
        let s = strip_jsonc(r#"{ "folder": "x", "somethingElse": 1 }"#);
        let cfg: ServeFileConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(cfg.folder.as_deref(), Some("x"));
    }

    #[test]
    fn upgrade_adds_missing_keys_preserving_the_file() {
        let p = std::env::temp_dir().join(format!("ft-upgrade-{}.json", std::process::id()));
        let p = p.to_str().unwrap();
        // An old, partial config WITH a comment, to prove comments survive.
        std::fs::write(p, "{\n  // my prod server\n  \"folder\": \"C:/data\",\n  \"port\": 9000\n}\n").unwrap();

        let cfg = ServeFileConfig::load(p).unwrap();
        assert_eq!(cfg.port, Some(9000), "user value preserved");

        let after = std::fs::read_to_string(p).unwrap();
        assert!(after.contains("// my prod server"), "original comment preserved");
        assert!(after.contains("\"folder\": \"C:/data\""), "original content preserved");
        assert!(after.contains("\"streams\""), "missing key added");
        assert!(after.contains("\"compressMargin\""), "missing key added");
        assert!(std::fs::read_to_string(format!("{p}.bak")).unwrap().contains("\"port\": 9000"), "backup written");

        // Still valid + values intact, defaults filled.
        let cfg2: ServeFileConfig = serde_json::from_str(&strip_jsonc(&after)).unwrap();
        assert_eq!(cfg2.port, Some(9000));
        assert_eq!(cfg2.streams, Some(4));

        // Idempotent: a second load must not change the file again.
        let _ = ServeFileConfig::load(p).unwrap();
        assert_eq!(after, std::fs::read_to_string(p).unwrap(), "second load must be a no-op");

        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(format!("{p}.bak"));
    }

    #[test]
    fn client_conn_round_trips() {
        let c = ClientConn {
            server: Some("1.2.3.4".into()),
            port: Some(8722),
            token: Some("abc".into()),
            fingerprint: Some("ff".into()),
            ignore: Some("log/".into()),
            streams: Some(4),
        };
        let json = c.to_json();
        let back = serde_json::from_str::<ClientConn>(&json).unwrap();
        assert_eq!(back.server.as_deref(), Some("1.2.3.4"));
        assert_eq!(back.streams, Some(4));
    }
}
