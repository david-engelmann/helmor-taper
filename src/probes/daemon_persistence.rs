//! Headless probe: fire `agent.send`, force a disconnect/reconnect,
//! verify the daemon's PID didn't change AND the agent session is
//! still registered with the daemon. Proves the persistent-daemon
//! invariant (the daemon is a double-forked child of init, survives
//! per-session SSH proxy churn).

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{sleep, Instant};

use crate::bridge::Bridge;
use crate::commands::{execute_js, invoke_and_wait};
use crate::probes::run_cmd;

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
    pub host_alias: String,
    pub remote_binary: String,
    pub container: String,
    pub local_workspace_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            host_alias: std::env::var("HOST_ALIAS")
                .unwrap_or_else(|_| "helmor-taper-arm64".into()),
            remote_binary: std::env::var("REMOTE_BINARY")
                .unwrap_or_else(|_| "/home/e2e/.helmor/server/helmor-server".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
            local_workspace_dir: std::env::var("LOCAL_WS_DIR").unwrap_or_else(|_| {
                "/Users/david/helmor-dev/workspaces/helmor-taper/aludra".into()
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceBinding {
    workspace_id: String,
    runtime_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSession {
    session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentSession {
    request_id: String,
}

fn daemon_pid(container: &str) -> Option<String> {
    run_cmd(
        "docker",
        &[
            "exec",
            container,
            "sh",
            "-c",
            "pgrep -f 'helmor-server.real --daemon' | head -1",
        ],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let timeout = Duration::from_secs(60);
    let bindings: Vec<WorkspaceBinding> = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "list_workspace_runtime_bindings",
            json!({}),
            timeout,
            "pdp-list",
        )
        .await?,
    )?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow::anyhow!("no workspace bound to {}", config.runtime_name))?;

    let session: CreateSession = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "create_session",
            json!({"workspaceId": bound.workspace_id}),
            timeout,
            "pdp-mksession",
        )
        .await?,
    )?;
    eprintln!(
        "✓ fresh helmor session {}…",
        &session.session_id[..session.session_id.len().min(8)]
    );

    invoke_and_wait(
        bridge,
        "update_app_settings",
        json!({
            "settingsMap": {
                "app.claude_custom_providers": serde_json::to_string(&json!({
                    "customBaseUrl": "http://host.docker.internal:1235",
                    "customApiKey": "lm-studio",
                    "customModels": "google/gemma-4-26b-a4b",
                }))?,
            }
        }),
        timeout,
        "pdp-settings",
    )
    .await?;

    let request = json!({
        "provider": "claude",
        "modelId": "claude-custom|custom|google/gemma-4-26b-a4b",
        "prompt": "Reply with exactly: REMOTE_AGENT_OK",
        "sessionId": Value::Null,
        "helmorSessionId": session.session_id,
        "workingDirectory": config.local_workspace_dir,
        "effortLevel": "medium",
        "permissionMode": "bypassPermissions",
        "fastMode": false,
    });
    let driver = format!(
        r#"
        window.__taper = window.__taper || {{}};
        var slot = (window.__taper.dp = {{ evs: [], done: false, error: null }});
        var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {{
            if (raw && 'end' in raw) {{ slot.done = true; return; }}
            slot.evs.push(raw && raw.message);
        }});
        var ch = {{ toJSON: function(){{ return "__CHANNEL__:" + id; }} }};
        var req = {req};
        var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", {{ request: req, onEvent: ch }});
        p["then"](function(){{}}, function(e){{ slot.error = String(e && e.message ? e.message : e); slot.done = true; }});
        return "started";"#,
        req = serde_json::to_string(&request)?,
    );
    execute_js(bridge, &driver).await?;
    eprintln!("✓ agent.send fired");

    let list_deadline = Instant::now() + Duration::from_secs(15);
    let mut sessions: Vec<AgentSession> = vec![];
    while Instant::now() < list_deadline {
        sessions = serde_json::from_value(
            invoke_and_wait(
                bridge,
                "list_remote_agent_sessions",
                json!({"name": config.runtime_name}),
                timeout,
                "pdp-list-sessions",
            )
            .await?,
        )?;
        if !sessions.is_empty() {
            break;
        }
        sleep(Duration::from_millis(400)).await;
    }
    if sessions.is_empty() {
        eprintln!("✗ no agent sessions registered with the daemon");
        return Ok(false);
    }
    let target = sessions.last().unwrap().request_id.clone();
    eprintln!(
        "✓ daemon has {} session(s); newest request_id={}…",
        sessions.len(),
        &target[..target.len().min(8)]
    );

    let pid_before = match daemon_pid(&config.container) {
        Some(p) => p,
        None => {
            eprintln!("✗ couldn't read daemon pid before disconnect");
            return Ok(false);
        }
    };
    eprintln!("✓ daemon pid before disconnect: {pid_before}");

    invoke_and_wait(
        bridge,
        "disconnect_remote_runtime",
        json!({"name": config.runtime_name}),
        Duration::from_secs(30),
        "pdp-disc",
    )
    .await?;
    sleep(Duration::from_millis(800)).await;
    eprintln!("✓ disconnected remote runtime");

    invoke_and_wait(
        bridge,
        "connect_remote_runtime",
        json!({
            "name": config.runtime_name,
            "host": config.host_alias,
            "remoteBinary": config.remote_binary,
            "forwardAgent": false,
        }),
        Duration::from_secs(30),
        "pdp-reconn",
    )
    .await?;
    eprintln!("✓ reconnected remote runtime");

    let pid_after = daemon_pid(&config.container).unwrap_or_default();
    let pid_same = pid_after == pid_before;
    eprintln!("✓ daemon pid after reconnect: {pid_after} (same as before: {pid_same})");

    let sessions_after: Vec<AgentSession> = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "list_remote_agent_sessions",
            json!({"name": config.runtime_name}),
            timeout,
            "pdp-list-after",
        )
        .await?,
    )?;
    let still_present = sessions_after.iter().any(|s| s.request_id == target);
    eprintln!(
        "✓ session still in daemon after reconnect: {still_present} ({} total)",
        sessions_after.len()
    );

    Ok(pid_same && still_present)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("HOST_ALIAS");
            std::env::remove_var("REMOTE_BINARY");
            std::env::remove_var("CONTAINER");
            std::env::remove_var("LOCAL_WS_DIR");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.host_alias, "helmor-taper-arm64");
    }

    #[test]
    fn agent_session_deserializes_camel_case() {
        let v = json!([{"requestId": "abc-123"}]);
        let parsed: Vec<AgentSession> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed[0].request_id, "abc-123");
    }
}
