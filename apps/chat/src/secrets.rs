//! Search API keys in the Secret Service (libsecret's secret-tool), not in settings.toml.

use std::io::Write;
use std::process::{Command, Stdio};

fn attributes(name: &str) -> [&str; 4] {
    ["application", "nyx-chat", "key", name]
}

/// The saved key, or empty when there's none (or no keyring).
pub fn get(name: &str) -> String {
    Command::new("secret-tool")
        .arg("lookup")
        .args(attributes(name))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Save `value` under `name`; an empty value removes it.
pub fn set(name: &str, value: &str) -> Result<(), String> {
    let failed = || "Unlock the desktop keyring and make sure libsecret (secret-tool) is installed.".to_string();
    if value.is_empty() {
        let ok = Command::new("secret-tool").arg("clear").args(attributes(name)).status().map_err(|_| failed())?.success();
        return if ok { Ok(()) } else { Err(failed()) };
    }
    let mut child = Command::new("secret-tool")
        .arg("store")
        .arg(format!("--label=Chat {name} search key"))
        .args(attributes(name))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| failed())?;
    child.stdin.take().ok_or_else(failed)?.write_all(value.as_bytes()).map_err(|_| failed())?;
    if child.wait().map_err(|_| failed())?.success() { Ok(()) } else { Err(failed()) }
}
