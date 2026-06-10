//! The flagship remote-runner scenario: connect over SSH, prove the
//! daemon comes up healthy, confirm the backend's `list_remote_runtimes`
//! reflects the connected state. A superset of [`connect_over_ssh`]
//! with backend-truth verification.
//!
//! Rust port of `scenarios/remote-runner.ts`. The TS port used an
//! absolute timeline (`at(ms)`) to align scenes with the recorder;
//! the Rust port drives the same beats via SceneSpec.hold_sec.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::scenarios::connect_over_ssh::{regex_like_semver_check_pub, DaemonHealth};
use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub host_alias: String,
    pub runtime_name: String,
    pub remote_binary: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            host_alias: std::env::var("HOST_ALIAS").unwrap_or_else(|_| "helmor-taper-arm64".into()),
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            remote_binary: std::env::var("REMOTE_BINARY")
                .unwrap_or_else(|_| "/home/e2e/.helmor/server/helmor-server".into()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RuntimeEntry {
    pub name: String,
    #[serde(rename = "isLocal", default)]
    pub is_local: bool,
    pub state: Option<RuntimeState>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RuntimeState {
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    let row_selector = format!("[data-testid=remote-server-row-{}]", config.runtime_name);

    // Clean slate.
    let _ = tape
        .invoke::<Value>(
            "disconnect_remote_runtime",
            json!({"name": config.runtime_name}),
        )
        .await;
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(500)).await;

    // Scene 1: empty panel.
    tape.open_settings("remote-servers").await?;
    tape.sleep(Duration::from_millis(800)).await;
    let panel_opens = tape
        .wait_for("[role=dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("panel_opens", panel_opens, "");
    let starts_empty = tape
        .wait_for("[data-testid=remote-servers-empty]", Duration::from_secs(3))
        .await?;
    tape.assert("starts_empty", starts_empty, "no remote servers yet");
    tape.scene(SceneSpec::new("Settings → Remote Servers: empty until we connect one").hold_sec(4))
        .await?;

    // Scene 2: SSH connect.
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(400)).await;
    let connect_start = std::time::Instant::now();
    let health: DaemonHealth = tape
        .invoke(
            "connect_remote_runtime",
            json!({
                "name": config.runtime_name,
                "host": config.host_alias,
                "remoteBinary": config.remote_binary,
                "forwardAgent": false,
            }),
        )
        .await?;
    let connect_ms = connect_start.elapsed().as_millis() as u64;
    tape.assert("ssh_connect_succeeds", true, format!("{connect_ms}ms"));

    let kind_remote = health
        .kind
        .as_ref()
        .and_then(|k| k.kind.as_deref())
        .is_some_and(|k| k == "remote");
    let kind_host_match = health
        .kind
        .as_ref()
        .and_then(|k| k.host.as_deref())
        .is_some_and(|h| h == config.host_alias);
    tape.assert(
        "daemon_reports_remote",
        kind_remote && kind_host_match,
        serde_json::to_string(&health.kind).unwrap_or_default(),
    );
    let version = health.version.clone().unwrap_or_default();
    tape.assert(
        "daemon_reports_version",
        regex_like_semver_check_pub(&version),
        format!("v{version}"),
    );
    let hostname = health.hostname.clone().unwrap_or_default();
    tape.assert(
        "daemon_reports_hostname",
        !hostname.is_empty(),
        hostname.clone(),
    );

    // Scene 3: reopen panel → connected row.
    tape.open_settings("remote-servers").await?;
    tape.sleep(Duration::from_millis(1500)).await;
    let row_visible = tape
        .wait_for(&row_selector, Duration::from_secs(10))
        .await?;
    tape.assert("ui_shows_connected_row", row_visible, "");
    let row_text_script = format!(
        r#"var r=document.querySelector({sel}); return r?r.innerText.replace(/\n+/g," | "):null;"#,
        sel = serde_json::to_string(&row_selector)?,
    );
    let row_text: Option<String> = tape.js(&row_text_script).await?;
    let says_connected = row_text
        .as_deref()
        .is_some_and(|t| t.to_lowercase().contains("connected"));
    tape.assert(
        "row_says_connected",
        says_connected,
        row_text.clone().unwrap_or_default(),
    );

    // Confirm backend truth.
    let runtimes: Vec<RuntimeEntry> = tape.invoke("list_remote_runtimes", json!({})).await?;
    let remote = runtimes.iter().find(|r| r.name == config.runtime_name);
    let backend_connected = remote
        .and_then(|r| r.state.as_ref())
        .and_then(|s| s.kind.as_deref())
        .is_some_and(|k| k == "connected");
    tape.assert(
        "backend_runtime_connected",
        backend_connected,
        serde_json::to_string(&remote.and_then(|r| r.state.as_ref())).unwrap_or_default(),
    );

    tape.scene(
        SceneSpec::new(format!(
            "Connected — helmor-server {} live on {} ({}ms)",
            version, hostname, connect_ms
        ))
        .hold_sec(6),
    )
    .await?;

    tape.finish(json!({
        "host": config.host_alias,
        "runtimeName": config.runtime_name,
        "remoteBinary": config.remote_binary,
        "connectMs": connect_ms,
        "health": serde_json::to_value(&health).ok(),
        "runtimeCount": runtimes.len(),
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("HOST_ALIAS");
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("REMOTE_BINARY");
        }
        let c = Config::from_env();
        assert_eq!(c.host_alias, "helmor-taper-arm64");
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.remote_binary, "/home/e2e/.helmor/server/helmor-server");
    }

    #[test]
    fn runtime_entry_deserializes_with_is_local() {
        let v = json!({
            "name": "local", "isLocal": true,
            "state": {"type": "connected"},
        });
        let r: RuntimeEntry = serde_json::from_value(v).unwrap();
        assert_eq!(r.name, "local");
        assert!(r.is_local);
        assert_eq!(r.state.and_then(|s| s.kind).as_deref(), Some("connected"));
    }
}
