//! Track E proof: from the dev-only Runtime Debug panel an operator
//! reads live SSH diagnostics (ping RTT + transport state), per-method
//! RPC metrics (counts/errors/p50/p99), one-clicks a support bundle to
//! the clipboard, and tails the remote daemon's log — all without
//! leaving Helmor or shelling into the host.
//!
//! Rust port of `scenarios/observability.ts`. Preconditions: a
//! connected remote runtime named `runtime_name` (defaults to the
//! docker fixture). The scenario discovers the runtime via
//! `list_remote_runtimes` so it survives renames.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuntimeEntry {
    pub name: String,
    pub state: Option<RuntimeState>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuntimeState {
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    let runtimes: Vec<RuntimeEntry> = tape.invoke("list_remote_runtimes", json!({})).await?;
    let remote = runtimes.iter().find(|r| r.name == config.runtime_name);
    let state_label = remote
        .and_then(|r| r.state.as_ref())
        .and_then(|s| s.kind.as_deref())
        .unwrap_or("<missing>");
    tape.assert(
        "remote_connected",
        state_label == "connected",
        format!("{}={state_label}", config.runtime_name),
    );

    tape.js::<Value>(r#"window.location.reload(); return "r";"#)
        .await?;
    tape.sleep(Duration::from_secs(6)).await;
    tape.open_settings("runtime-debug").await?;
    let panel_opens = tape
        .wait_for("[role=dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("panel_opens", panel_opens, "");

    // Connection diagnostics card.
    let diagnostics_card = tape
        .wait_for(
            "[data-testid=connection-diagnostics-card]",
            Duration::from_secs(10),
        )
        .await?;
    tape.assert("diagnostics_card", diagnostics_card, "");
    let ping_rtt = tape
        .wait_for("[data-testid=diagnostics-ping-ms]", Duration::from_secs(8))
        .await?;
    tape.assert("ping_rtt_shown", ping_rtt, "");
    tape.scroll_to_section("[data-testid=connection-diagnostics-card]")
        .await?;
    tape.sleep(Duration::from_millis(800)).await;
    let diag_text: Option<String> = tape
        .js(
            r#"var c=document.querySelector('[data-testid=connection-diagnostics-card]');
               return c?c.innerText.replace(/\n+/g," · ").slice(0,200):null;"#,
        )
        .await?;
    tape.log(&format!(
        "diagnostics: {}",
        diag_text.clone().unwrap_or_default()
    ));
    tape.scene(
        SceneSpec::new(format!(
            "Runtime Debug → Connection diagnostics: live SSH ping RTT, protocol handshake & transport state for {}",
            config.runtime_name
        ))
        .hold_sec(5),
    )
    .await?;

    // Per-method metrics table.
    let metrics_table = tape
        .wait_for(
            "[data-testid=runtime-metrics-table]",
            Duration::from_secs(10),
        )
        .await?;
    tape.assert("metrics_table", metrics_table, "");
    tape.scroll_to_section("[data-testid=runtime-metrics-runtime-select]")
        .await?;
    tape.sleep(Duration::from_millis(800)).await;
    let method_count: u64 = tape
        .js(
            r#"var t=document.querySelector('[data-testid=runtime-metrics-table] tbody');
               return t?t.querySelectorAll('tr').length:0;"#,
        )
        .await?;
    tape.assert(
        "metrics_have_rows",
        method_count > 0,
        format!("{method_count} methods"),
    );
    tape.scene(
        SceneSpec::new(
            "Per-method RPC metrics: call counts, error counts, p50/p99 latency — read straight from the remote daemon",
        )
        .hold_sec(5),
    )
    .await?;

    // Copy-diagnostics bundle.
    tape.click("[data-testid=runtime-metrics-copy]").await?;
    let toast = tape
        .wait_for("[data-sonner-toast]", Duration::from_secs(5))
        .await?;
    tape.assert(
        "copy_toast",
        toast,
        if toast {
            "toast shown"
        } else {
            "no toast (clipboard may be denied in webview)"
        },
    );
    tape.scene(
        SceneSpec::new(
            "Copy diagnostics: one click bundles health + metrics + the last 50 daemon-log lines into a JSON blob for a support thread",
        )
        .record_sec(2)
        .hold_sec(5),
    )
    .await?;

    // Daemon log tail.
    let daemon_log_pre = tape
        .wait_for("[data-testid=daemon-log-pre]", Duration::from_secs(10))
        .await?;
    tape.assert("daemon_log_pre", daemon_log_pre, "");
    tape.scroll_to_section("[data-testid=daemon-log-runtime-select]")
        .await?;
    tape.sleep(Duration::from_millis(800)).await;
    let log_len: u64 = tape
        .js(
            r#"var p=document.querySelector('[data-testid=daemon-log-pre]');
               return p?p.innerText.split("\n").length:0;"#,
        )
        .await?;
    tape.assert(
        "daemon_log_has_lines",
        log_len > 0,
        format!("{log_len} lines"),
    );
    tape.scene(
        SceneSpec::new(format!(
            "Daemon log: tail $HOME/.helmor/server/daemon.log on {} without SSHing in — the first stop when an agent.send errors",
            config.runtime_name
        ))
        .hold_sec(5),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "methodCount": method_count,
        "logLen": log_len,
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_entry_deserializes_connected_state() {
        let v = json!({
            "name": "docker-linux-arm64",
            "state": {"type": "connected"},
        });
        let parsed: RuntimeEntry = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.name, "docker-linux-arm64");
        assert_eq!(
            parsed.state.and_then(|s| s.kind).as_deref(),
            Some("connected")
        );
    }

    #[test]
    fn runtime_entry_with_missing_state_decodes_to_default() {
        let v = json!({"name": "docker-linux-arm64"});
        let parsed: RuntimeEntry = serde_json::from_value(v).unwrap();
        assert!(parsed.state.is_none());
    }

    #[test]
    fn runtime_entry_serde_default_label_is_arm64() {
        unsafe { std::env::remove_var("RUNTIME_NAME") };
        assert_eq!(Config::from_env().runtime_name, "docker-linux-arm64");
    }
}
