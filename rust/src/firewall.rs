//! Firewall opening: Windows best-effort via `netsh` (equivalent to the
//! PowerShell `New-NetFirewallRule`), a no-op everywhere else. The rule is
//! removed when the guard drops (normal exit). See spec sections 2 and 7.3.

/// Holds an opened firewall rule and removes it on drop.
pub struct FirewallGuard {
    // Only read on Windows (in `Drop` via `delete_rule`); a no-op elsewhere.
    #[cfg_attr(not(windows), allow(dead_code))]
    rule: String,
    opened: bool,
}

impl Drop for FirewallGuard {
    fn drop(&mut self) {
        if self.opened {
            #[cfg(windows)]
            delete_rule(&self.rule);
        }
    }
}

/// Open inbound TCP `port` (optionally scoped to `allow_ip`). Best-effort:
/// logs a warning and continues if it cannot (e.g. not elevated).
pub fn open(port: u16, allow_ip: Option<&str>) -> FirewallGuard {
    let rule = format!("ft-temp-{port}");
    let opened = open_impl(&rule, port, allow_ip);
    FirewallGuard { rule, opened }
}

#[cfg(windows)]
fn open_impl(rule: &str, port: u16, allow_ip: Option<&str>) -> bool {
    use std::process::Command;
    // Clean any rule left over from a previous crashed run, then open fresh.
    delete_rule(rule);
    let mut args = vec![
        "advfirewall".to_string(),
        "firewall".to_string(),
        "add".to_string(),
        "rule".to_string(),
        format!("name={rule}"),
        "dir=in".to_string(),
        "action=allow".to_string(),
        "protocol=TCP".to_string(),
        format!("localport={port}"),
    ];
    if let Some(ip) = allow_ip {
        args.push(format!("remoteip={ip}"));
    }
    match Command::new("netsh").args(&args).output() {
        Ok(o) if o.status.success() => {
            eprintln!(
                "[ft] firewall OPENED: TCP {port} {}",
                allow_ip.map(|i| format!("from {i}")).unwrap_or_else(|| "(any source)".into())
            );
            true
        }
        _ => {
            eprintln!(
                "[ft] WARN: could not open firewall (run as Administrator to auto-open TCP {port}, \
                 open it manually, or pass --no-firewall to silence this)."
            );
            false
        }
    }
}

#[cfg(not(windows))]
fn open_impl(_rule: &str, port: u16, _allow_ip: Option<&str>) -> bool {
    eprintln!("[ft] firewall: no-op on this platform (ensure TCP {port} is reachable if needed).");
    false
}

#[cfg(windows)]
fn delete_rule(rule: &str) {
    use std::process::Command;
    let _ = Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule", &format!("name={rule}")])
        .output();
}
