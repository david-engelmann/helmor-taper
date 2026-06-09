//! Headless probe: fire `send_agent_message_stream` against a workspace
//! bound to the remote runtime, configure LM Studio as the custom
//! provider, count + print streamed events. Pass = at least one event
//! came back with no error.

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
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub prompt: String,
    pub local_workspace_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            model: std::env::var("MODEL").unwrap_or_else(|_| "google/gemma-4-26b-a4b".into()),
            base_url: std::env::var("LM_STUDIO_BASE")
                .unwrap_or_else(|_| "http://host.docker.internal:1235".into()),
            api_key: std::env::var("LM_STUDIO_KEY").unwrap_or_else(|_| "lm-studio".into()),
            prompt: std::env::var("PROMPT")
                .unwrap_or_else(|_| "Reply with exactly: REMOTE_AGENT_OK".into()),
            local_workspace_dir: std::env::var("LOCAL_WS_DIR").unwrap_or_else(|_| {
                "/Users/david/helmor-dev/workspaces/helmor-taper/albiorix".into()
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
struct CreateSession {
    session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaperState {
    #[serde(default)]
    defined: bool,
    #[serde(default)]
    n: u64,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let timeout = Duration::from_secs(60);

    let provider_json = serde_json::to_string(&json!({
        "customBaseUrl": config.base_url,
        "customApiKey": config.api_key,
        "customModels": config.model,
    }))?;
    invoke_and_wait(
        bridge,
        "update_app_settings",
        json!({"settingsMap": {"app.claude_custom_providers": provider_json}}),
        timeout,
        "probe-settings",
    )
    .await?;
    eprintln!("✓ provider set: {} · {}", config.base_url, config.model);

    let bindings: Vec<WorkspaceBinding> = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "list_workspace_runtime_bindings",
            json!({}),
            timeout,
            "probe-bindings",
        )
        .await?,
    )?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow::anyhow!("no workspace bound to {}", config.runtime_name))?;
    let fresh: CreateSession = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "create_session",
            json!({"workspaceId": bound.workspace_id}),
            timeout,
            "probe-mksession",
        )
        .await?,
    )?;
    eprintln!(
        "✓ ws={}… session={}… (fresh) remote={}",
        &bound.workspace_id[..bound.workspace_id.len().min(8)],
        &fresh.session_id[..fresh.session_id.len().min(8)],
        bound.remote_path
    );

    let model_id = format!("claude-custom|custom|{}", config.model);
    let request = json!({
        "provider": "claude",
        "modelId": model_id,
        "prompt": config.prompt,
        "sessionId": Value::Null,
        "helmorSessionId": fresh.session_id,
        "workingDirectory": config.local_workspace_dir,
        "effortLevel": "medium",
        "permissionMode": "bypassPermissions",
        "fastMode": false,
    });
    let driver = format!(
        r#"
        window.__taper = window.__taper || {{}};
        window.__taper.evs = [];
        window.__taper.done = false;
        window.__taper.error = null;
        var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {{
            if (raw && 'end' in raw) {{ window.__taper.done = true; return; }}
            window.__taper.evs.push(raw && raw.message);
        }});
        var onEvent = {{ toJSON: function(){{ return "__CHANNEL__:" + id; }} }};
        var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", {{ request: {req}, onEvent: onEvent }});
        p["then"](function(){{}}, function(e){{ window.__taper.error = String((e && e.message) ? e.message : e); window.__taper.done = true; }});
        return "started";"#,
        req = serde_json::to_string(&request)?,
    );
    execute_js(bridge, &driver).await?;
    eprintln!("✓ agent.send fired (model={model_id})");

    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        let snap: TaperState = serde_json::from_value(
            execute_js(
                bridge,
                r#"var t=window.__taper||{}; return { defined: !!window.__taper, n: (t.evs||[]).length, done: !!t.done, error: t.error||null };"#,
            )
            .await?,
        )?;
        if !snap.defined {
            eprintln!("✗ window.__taper vanished");
            return Ok(false);
        }
        if let Some(e) = snap.error {
            eprintln!("✗ agent.send rejected: {e}");
            return Ok(false);
        }
        if snap.done {
            eprintln!("\nfinal: {} events streamed back", snap.n);
            return Ok(snap.n > 0);
        }
        sleep(Duration::from_millis(500)).await;
    }
    eprintln!("✗ deadline elapsed before stream completed");
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("MODEL");
            std::env::remove_var("LM_STUDIO_BASE");
            std::env::remove_var("LM_STUDIO_KEY");
            std::env::remove_var("PROMPT");
            std::env::remove_var("LOCAL_WS_DIR");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.model, "google/gemma-4-26b-a4b");
        assert_eq!(c.base_url, "http://host.docker.internal:1235");
    }
}
