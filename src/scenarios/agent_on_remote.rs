//! THE flagship agent-runs-on-the-container tape: fires `send_agent_message_stream`
//! via a hand-rolled Tauri Channel so the daemon's per-session journal
//! populates, then captures the Remote agent sessions row appearing in
//! the Runtime Debug panel with live "last event" metadata.
//!
//! Rust port of `scenarios/agent-on-remote.ts`. The hand-rolled
//! Channel callback (via `window.__TAURI_INTERNALS__.transformCallback`)
//! is the trick that lets the scenario invoke a streaming Tauri command
//! from inside the webview without awaiting it — the scenario doesn't
//! care about the actual streamed bytes, only that the row + journal
//! show evidence of the daemon doing real work.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
    pub local_workspace_dir: String,
    pub prompt: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            local_workspace_dir: std::env::var("LOCAL_WS_DIR").unwrap_or_else(|_| {
                "/Users/david/helmor-dev/workspaces/helmor-taper/aludra".into()
            }),
            prompt: std::env::var("PROMPT").unwrap_or_else(|_| {
                "In one short sentence, explain what makes a remote development environment isolated.".into()
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
struct CreateSessionResult {
    session_id: String,
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    // 0. Configure LM Studio bridge so agent.send doesn't hit Anthropic.
    tape.invoke::<Value>(
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
    )
    .await?;

    // 1. Find the workspace bound to the remote + create a fresh session.
    let bindings: Vec<WorkspaceBinding> = tape
        .invoke("list_workspace_runtime_bindings", json!({}))
        .await?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow!("no workspace bound to {}", config.runtime_name))?;
    let session: CreateSessionResult = tape
        .invoke(
            "create_session",
            json!({"workspaceId": bound.workspace_id}),
        )
        .await?;
    tape.log(&format!(
        "workspace {}… → {}; fresh session {}…",
        &bound.workspace_id[..bound.workspace_id.len().min(8)],
        bound.remote_path,
        &session.session_id[..session.session_id.len().min(8)],
    ));

    // Open Runtime Debug + scroll to the Remote agent sessions section.
    tape.js::<Value>(r#"window.location.reload(); return "r";"#).await?;
    tape.sleep(Duration::from_secs(6)).await;
    tape.open_settings("runtime-debug").await?;
    let panel_open = tape
        .wait_for("[role=dialog]", Duration::from_secs(10))
        .await?;
    tape.assert("panel_opens", panel_open, "");
    // Section heading scroll (no testid on the section root — match h3 text).
    tape.js::<bool>(
        r#"var hs=document.querySelectorAll('h3');
           for(var i=0;i<hs.length;i++){
             if(/Remote agent sessions/i.test(hs[i].textContent||'')){
               (hs[i].closest('section')||hs[i]).scrollIntoView({block:'start',behavior:'auto'});
               return true;
             }
           } return false;"#,
    )
    .await?;
    tape.sleep(Duration::from_millis(500)).await;

    tape.scene(
        SceneSpec::new(format!(
            "Runtime Debug → Remote agent sessions: no agent has run yet on {}",
            config.runtime_name
        ))
        .hold_sec(4),
    )
    .await?;

    // 2. Fire send_agent_message_stream via a hand-rolled Tauri Channel
    //    callback. transformCallback returns a numeric id; we wrap it in
    //    an object with a toJSON() so it serializes as `__CHANNEL__:<id>`
    //    — the wire shape Tauri's IPC expects for a Channel arg.
    let request_payload = json!({
        "provider": "claude",
        "modelId": "claude-custom|custom|google/gemma-4-26b-a4b",
        "prompt": config.prompt,
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
        var slot = (window.__taper.send = {{ evs: [], done: false, error: null }});
        var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {{
            if (raw && 'end' in raw) {{ slot.done = true; return; }}
            slot.evs.push(raw && raw.message);
        }});
        var ch = {{ toJSON: function(){{ return "__CHANNEL__:" + id; }} }};
        var req = {req};
        var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", {{ request: req, onEvent: ch }});
        p["then"](function(){{}}, function(e){{ slot.error = String(e && e.message ? e.message : e); slot.done = true; }});
        return "started";"#,
        req = serde_json::to_string(&request_payload)?,
    );
    tape.js::<Value>(&driver).await?;
    tape.log("agent.send fired in background");

    // Wait for the row to appear.
    let row_deadline = Instant::now() + Duration::from_secs(30);
    let mut request_id: Option<String> = None;
    while Instant::now() < row_deadline {
        let candidate: Option<String> = tape
            .js(
                r#"var row=document.querySelector('[data-testid^=remote-agent-session-]');
                   if(!row) return null;
                   return (row.getAttribute('data-testid')||'').replace(/^remote-agent-session-/, '');"#,
            )
            .await?;
        if let Some(rid) = candidate {
            if !rid.is_empty() {
                request_id = Some(rid);
                break;
            }
        }
        tape.sleep(Duration::from_millis(400)).await;
    }
    tape.assert(
        "agent_session_row_visible",
        request_id.is_some(),
        request_id.clone().unwrap_or_else(|| "(missing)".into()),
    );

    // Accelerate the panel's poll.
    let _ = tape.click("[aria-label^='Refresh agent sessions']").await;

    tape.scene(
        SceneSpec::new(format!(
            "agent.send → daemon spawned the sidecar in the container — a session row appears, request {}",
            request_id.as_deref().unwrap_or("").chars().take(8).collect::<String>()
        ))
        .record_sec(3)
        .hold_sec(5),
    )
    .await?;

    // 3. Poll the row summary until it shows "last event" → confirms
    //    the agent did real work, not just placeholder rendering.
    let summary_deadline = Instant::now() + Duration::from_secs(30);
    let mut row_summary: Option<String> = None;
    while Instant::now() < summary_deadline {
        let candidate: Option<String> = tape
            .js(
                r#"var row=document.querySelector('[data-testid^=remote-agent-session-]');
                   return row?row.innerText.replace(/\n+/g, ' · ').slice(0, 240):null;"#,
            )
            .await?;
        if let Some(s) = candidate.clone() {
            if s.contains("last event") {
                row_summary = candidate;
                break;
            }
        }
        row_summary = candidate;
        tape.sleep(Duration::from_millis(400)).await;
    }
    let has_last_event = row_summary
        .as_deref()
        .is_some_and(|s| s.contains("last event"));
    tape.assert(
        "row_shows_recent_activity",
        has_last_event,
        row_summary
            .clone()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect::<String>(),
    );
    tape.log(&format!(
        "row summary: {}",
        row_summary.clone().unwrap_or_default()
    ));

    // Let the agent stream a bit more before the final scene.
    tape.sleep(Duration::from_secs(6)).await;

    tape.scene(
        SceneSpec::new(
            "Row shows the live session: provider, workspace dir, last-event time — every byte came from claude running in the container",
        )
        .hold_sec(6),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "requestId": request_id,
        "rowSummary": row_summary,
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("LOCAL_WS_DIR");
            std::env::remove_var("PROMPT");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert!(c.local_workspace_dir.starts_with("/Users/david"));
        assert!(c.prompt.contains("isolated"));
    }

    #[test]
    fn create_session_result_deserializes_camel_case() {
        let v = json!({"sessionId": "abc-123"});
        let r: CreateSessionResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.session_id, "abc-123");
    }
}
