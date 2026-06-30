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
}

impl ServeFileConfig {
    pub fn load(path: &str) -> Result<Self, BoxError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("config file not found / unreadable: {path}: {e}"))?;
        let stripped = strip_jsonc(&raw);
        serde_json::from_str(&stripped).map_err(|e| {
            format!(
                "invalid JSON in {path}: {e}\n  Tip: use forward slashes \"C:/path\" or DOUBLED \
                 backslashes \"C:\\\\path\", and no trailing commas. ( // and /* */ comments are allowed. )"
            )
            .into()
        })
    }
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
