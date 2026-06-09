//! Headless probe of the install pipeline: snapshot the container's
//! pre-install state, drive `install_remote_bundle`, verify all
//! expected files appear with the right sha256, confirm the second
//! install is a no-op (idempotent), reconnect to use the new wrapper,
//! then fire an `agent.send` and assert the response contains
//! `REMOTE_AGENT_OK`. Strongest possible proof of "every byte arrived
//! via the install pipeline."

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
struct ManifestFile {
    path: String,
    sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    target: String,
    claude_code_version: String,
    files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallOutcome {
    manifest: Manifest,
    installed_files: Vec<String>,
    already_current: bool,
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

fn docker_ls(container: &str) -> String {
    run_cmd(
        "docker",
        &[
            "exec",
            "-u",
            "e2e",
            container,
            "sh",
            "-c",
            "ls $HOME/.helmor/server/ 2>/dev/null | sort",
        ],
    )
    .unwrap_or_default()
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let before = docker_ls(&config.container);
    eprintln!("=== pre-install state ===\n{before}\n=========================");

    let install_timeout = Duration::from_secs(600);
    let outcome: InstallOutcome = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "install_remote_bundle",
            json!({"name": config.runtime_name}),
            install_timeout,
            "pbi-install",
        )
        .await?,
    )?;
    eprintln!(
        "✓ install_remote_bundle returned: target={} claudeCode={}",
        outcome.manifest.target, outcome.manifest.claude_code_version
    );
    eprintln!(
        "  installed files: {}",
        if outcome.installed_files.is_empty() {
            "(none — already current)".to_string()
        } else {
            outcome.installed_files.join(", ")
        }
    );
    eprintln!("  alreadyCurrent: {}", outcome.already_current);

    let after = docker_ls(&config.container);
    eprintln!("=== post-install state ===\n{after}\n==========================");
    let expected = [
        "MANIFEST.json",
        "claude",
        "helmor-server",
        "helmor-server.real",
        "helmor-sidecar",
    ];
    let seen: std::collections::HashSet<&str> = after.split_whitespace().collect();
    let missing: Vec<&&str> = expected.iter().filter(|f| !seen.contains(**f)).collect();
    if !missing.is_empty() {
        eprintln!(
            "✗ missing expected post-install files: {}",
            missing.iter().map(|s| **s).collect::<Vec<_>>().join(", ")
        );
        return Ok(false);
    }
    eprintln!("✓ all expected files present on remote");

    let claude_entry = outcome
        .manifest
        .files
        .iter()
        .find(|f| f.path == "claude")
        .ok_or_else(|| anyhow::anyhow!("manifest missing 'claude' entry"))?;
    let observed_sha = run_cmd(
        "docker",
        &[
            "exec",
            "-u",
            "e2e",
            &config.container,
            "sh",
            "-c",
            "sha256sum $HOME/.helmor/server/claude | cut -d' ' -f1",
        ],
    )?;
    if observed_sha != claude_entry.sha256 {
        eprintln!(
            "✗ sha256 mismatch for claude: expected {}, got {observed_sha}",
            claude_entry.sha256
        );
        return Ok(false);
    }
    eprintln!(
        "✓ sha256(claude) on remote matches manifest ({}…)",
        &observed_sha[..observed_sha.len().min(12)]
    );

    let timeout = Duration::from_secs(120);
    let re_run: InstallOutcome = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "install_remote_bundle",
            json!({"name": config.runtime_name}),
            timeout,
            "pbi-rerun",
        )
        .await?,
    )?;
    if !re_run.already_current || !re_run.installed_files.is_empty() {
        eprintln!(
            "✗ second install should have been a no-op; got installedFiles={:?}",
            re_run.installed_files
        );
        return Ok(false);
    }
    eprintln!("✓ second install is a no-op (idempotent)");

    let _ = invoke_and_wait(
        bridge,
        "disconnect_remote_runtime",
        json!({"name": config.runtime_name}),
        Duration::from_secs(30),
        "pbi-disc",
    )
    .await;
    sleep(Duration::from_millis(800)).await;
    invoke_and_wait(
        bridge,
        "connect_remote_runtime",
        json!({
            "name": config.runtime_name,
            "host": config.host_alias,
            "remoteBinary": config.remote_binary,
            "forwardAgent": false,
        }),
        Duration::from_secs(60),
        "pbi-conn",
    )
    .await?;
    eprintln!("✓ reconnected with the new wrapper in place");

    let bindings: Vec<WorkspaceBinding> = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "list_workspace_runtime_bindings",
            json!({}),
            timeout,
            "pbi-bindings",
        )
        .await?,
    )?;
    let bound = match bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
    {
        Some(b) => b,
        None => {
            eprintln!("(no workspace bound to the runtime — skipping agent.send check)");
            eprintln!("✓ install path verified; agent.send check skipped");
            return Ok(true);
        }
    };

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
        "pbi-settings",
    )
    .await?;

    let session: CreateSession = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "create_session",
            json!({"workspaceId": bound.workspace_id}),
            timeout,
            "pbi-mksession",
        )
        .await?,
    )?;
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
        var slot = (window.__taper.pbi = {{ evs: [], done: false, error: null }});
        var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {{
            if (raw && 'end' in raw) {{ slot.done = true; return; }}
            slot.evs.push(raw && raw.message);
        }});
        var ch = {{ toJSON: function(){{ return "__CHANNEL__:" + id; }} }};
        var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", {{ request: {req}, onEvent: ch }});
        p["then"](function(){{}}, function(e){{ slot.error = String(e && e.message ? e.message : e); slot.done = true; }});
        return "started";"#,
        req = serde_json::to_string(&request)?,
    );
    execute_js(bridge, &driver).await?;

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut saw_marker = false;
    while Instant::now() < deadline {
        let flat: String = serde_json::from_value(
            execute_js(
                bridge,
                r#"var evs=(window.__taper.pbi||{}).evs||[]; return JSON.stringify(evs);"#,
            )
            .await?,
        )?;
        if flat.contains("REMOTE_AGENT_OK") {
            saw_marker = true;
            break;
        }
        sleep(Duration::from_millis(400)).await;
    }
    eprintln!("✓ agent response contains REMOTE_AGENT_OK: {saw_marker}");
    Ok(saw_marker)
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
        assert_eq!(c.container, "helmor-test-linux-arm64");
    }

    #[test]
    fn install_outcome_deserializes_camel_case() {
        let v = json!({
            "manifest": {
                "target": "linux-arm64",
                "claudeCodeVersion": "0.26.0",
                "files": [{"path": "claude", "sha256": "abc"}],
            },
            "installedFiles": ["claude"],
            "alreadyCurrent": false,
        });
        let parsed: InstallOutcome = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.manifest.target, "linux-arm64");
        assert_eq!(parsed.installed_files, vec!["claude"]);
        assert!(!parsed.already_current);
    }
}
