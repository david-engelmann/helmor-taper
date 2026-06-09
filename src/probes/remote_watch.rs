//! Headless probe: subscribe to UI mutations, start a workspace watch,
//! plant a file inside the container, assert the desktop receives a
//! `workspaceFilesChanged` event for the bound workspace. Without
//! the remote watcher the event would never fire (the local walker
//! would only see laptop-side changes).

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
    pub container: String,
    pub local_workspace_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
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
    remote_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartWatchResult {
    #[serde(default)]
    kind: String,
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let timeout = Duration::from_secs(60);
    let bindings: Vec<WorkspaceBinding> = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "list_workspace_runtime_bindings",
            json!({}),
            timeout,
            "prw-list",
        )
        .await?,
    )?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow::anyhow!("no workspace bound to {}", config.runtime_name))?;
    eprintln!(
        "✓ workspace {}… → {}",
        &bound.workspace_id[..bound.workspace_id.len().min(8)],
        bound.remote_path
    );

    let sub_id = uuid::Uuid::new_v4().to_string();
    let subscribe = format!(
        r#"
        window.__taper = window.__taper || {{}};
        var slot = (window.__taper.watch = {{ events: [], done: false, error: null, subscriptionId: {sid} }});
        var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {{
            if (raw && 'end' in raw) {{ slot.done = true; return; }}
            slot.events.push(raw && raw.message);
        }});
        var ch = {{ toJSON: function(){{ return "__CHANNEL__:" + id; }} }};
        var p = window.__TAURI_INTERNALS__.invoke("subscribe_ui_mutations", {{ subscriptionId: {sid}, onEvent: ch }});
        p["then"](function(){{}}, function(e){{ slot.error = String(e && e.message ? e.message : e); }});
        return "subscribed";"#,
        sid = serde_json::to_string(&sub_id)?,
    );
    execute_js(bridge, &subscribe).await?;
    sleep(Duration::from_millis(400)).await;
    eprintln!("✓ subscribed to ui-mutations channel");

    let _ = invoke_and_wait(
        bridge,
        "stop_workspace_watch",
        json!({"workspaceId": bound.workspace_id}),
        timeout,
        "prw-stop-prior",
    )
    .await;

    let start_result: StartWatchResult = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "start_workspace_watch",
            json!({
                "workspaceId": bound.workspace_id,
                "workspaceDir": config.local_workspace_dir,
            }),
            timeout,
            "prw-start",
        )
        .await?,
    )?;
    eprintln!("✓ start_workspace_watch → kind={}", start_result.kind);
    if start_result.kind != "remote" {
        eprintln!("✗ expected remote watcher, got {}", start_result.kind);
        return Ok(false);
    }

    let marker = format!("WATCHER_PROOF_{}.txt", std::process::id());
    let plant_cmd = format!(
        "printf 'remote-watcher-proof' > '{}/{marker}'",
        bound.remote_path
    );
    run_cmd(
        "docker",
        &["exec", &config.container, "sh", "-c", &plant_cmd],
    )?;
    eprintln!("✓ planted {marker} on container");

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw = false;
    let mut received: Vec<Value> = vec![];
    while Instant::now() < deadline {
        received = serde_json::from_value(
            execute_js(bridge, r#"return (window.__taper.watch.events || []);"#).await?,
        )?;
        saw = received.iter().any(|e| {
            e.get("type").and_then(Value::as_str) == Some("workspaceFilesChanged")
                && e.get("workspaceId").and_then(Value::as_str) == Some(&bound.workspace_id)
        });
        if saw {
            break;
        }
        sleep(Duration::from_millis(400)).await;
    }
    eprintln!("\nfilewatch event observed: {saw}");
    if !saw {
        eprintln!("recent events:");
        for ev in received.iter().rev().take(10).rev() {
            let s = serde_json::to_string(ev).unwrap_or_default();
            eprintln!("  · {}", s.chars().take(180).collect::<String>());
        }
    }

    let _ = invoke_and_wait(
        bridge,
        "stop_workspace_watch",
        json!({"workspaceId": bound.workspace_id}),
        timeout,
        "prw-stop-final",
    )
    .await;
    let _ = invoke_and_wait(
        bridge,
        "unsubscribe_ui_mutations",
        json!({"subscriptionId": sub_id}),
        timeout,
        "prw-unsubscribe",
    )
    .await;

    Ok(saw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("CONTAINER");
            std::env::remove_var("LOCAL_WS_DIR");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert!(c.local_workspace_dir.contains("helmor-taper"));
    }
}
