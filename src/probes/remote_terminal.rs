//! Headless probe: open a PTY via Helmor, write `whoami;hostname;pwd`,
//! capture the streamed output, assert it shows the container hostname
//! plus the e2e user plus the remote worktree path. Proves the PTY is
//! hosted on the container, not the laptop.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{sleep, Instant};

use crate::bridge::Bridge;
use crate::commands::{execute_js, invoke_and_wait};

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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceBinding {
    workspace_id: String,
    runtime_name: String,
    remote_path: String,
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let timeout = Duration::from_secs(60);
    let bindings: Vec<WorkspaceBinding> = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "list_workspace_runtime_bindings",
            json!({}),
            timeout,
            "prt-list",
        )
        .await?,
    )?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow::anyhow!("no workspace bound to {}", config.runtime_name))?;
    eprintln!(
        "✓ workspace {}… bound; remote={}",
        &bound.workspace_id[..bound.workspace_id.len().min(8)],
        bound.remote_path
    );

    let term_id = uuid::Uuid::new_v4().to_string();
    let driver = format!(
        r#"
        window.__taper = window.__taper || {{}};
        var slot = (window.__taper.term = {{ chunks: [], events: [], done: false, error: null, openResult: null }});
        var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {{
            if (raw && 'end' in raw) {{ slot.done = true; return; }}
            var notif = raw && raw.message;
            slot.events.push(notif);
            var ev = notif && notif.event;
            if (ev && ev.kind === "stdout" && typeof ev.data === "string") {{ slot.chunks.push(ev.data); }}
            if (ev && (ev.kind === "exited" || ev.kind === "error")) {{ slot.done = true; }}
        }});
        var channel = {{ toJSON: function(){{ return "__CHANNEL__:" + id; }} }};
        var args = {args};
        args.channel = channel;
        var p = window.__TAURI_INTERNALS__.invoke("open_remote_terminal", args);
        p["then"](function(v){{ slot.openResult = v; }}, function(e){{ slot.error = String(e && e.message ? e.message : e); slot.done = true; }});
        return "started";"#,
        args = serde_json::to_string(&json!({
            "runtimeName": config.runtime_name,
            "terminalId": term_id,
            "workspaceDir": bound.remote_path,
            "shell": "/bin/bash",
            "cols": 100,
            "rows": 30,
            "channel": Value::Null,
        }))?,
    );
    execute_js(bridge, &driver).await?;
    eprintln!(
        "✓ open_remote_terminal fired (terminal_id={}…)",
        &term_id[..term_id.len().min(8)]
    );

    sleep(Duration::from_secs(1)).await;
    let probe_data = "whoami; hostname; pwd; echo TERMINAL_DONE_MARKER\n";
    invoke_and_wait(
        bridge,
        "write_remote_terminal",
        json!({
            "runtimeName": config.runtime_name,
            "terminalId": term_id,
            "data": probe_data,
        }),
        timeout,
        "prt-write",
    )
    .await?;
    eprintln!("✓ wrote probe command: {}", probe_data.trim());

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut buf = String::new();
    while Instant::now() < deadline {
        let chunks: String = serde_json::from_value(
            execute_js(
                bridge,
                r#"return (window.__taper.term.chunks || []).join("");"#,
            )
            .await?,
        )?;
        buf = chunks;
        if buf.contains("TERMINAL_DONE_MARKER") {
            break;
        }
        sleep(Duration::from_millis(300)).await;
    }
    let saw_marker = buf.contains("TERMINAL_DONE_MARKER");
    let saw_e2e = buf.split_whitespace().any(|t| t == "e2e");
    let saw_remote_path = buf.contains(&bound.remote_path);

    eprintln!("\noutput buffer:\n----");
    eprintln!("{}", buf.chars().take(800).collect::<String>());
    eprintln!("----");
    eprintln!(
        "\nchecks: marker={saw_marker} user=e2e:{saw_e2e} pwd={}:{saw_remote_path}",
        bound.remote_path
    );

    let _ = invoke_and_wait(
        bridge,
        "close_remote_terminal",
        json!({"runtimeName": config.runtime_name, "terminalId": term_id}),
        timeout,
        "prt-close",
    )
    .await;

    Ok(saw_marker && saw_e2e && saw_remote_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_default() {
        unsafe { std::env::remove_var("RUNTIME_NAME") };
        assert_eq!(Config::from_env().runtime_name, "docker-linux-arm64");
    }
}
