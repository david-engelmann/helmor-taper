//! Real chat thread, driven through the composer. Proves: a user
//! typing a prompt in the Helmor chat surface gets an answer that
//! ONLY makes sense if the agent ran on the remote container.
//!
//! Rust port of `scenarios/chat-real-on-remote.ts`. Three beats:
//! 1. List the workspace files → assistant's reply includes
//!    REMOTE_ONLY_MARKER (the marker only exists on the container).
//! 2. Read README.md's second line → quotes a line from the container
//!    copy, not the laptop one.
//! 3. Create a new file → file lands on the container's disk.

use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
    pub host_alias: String,
    pub remote_binary: String,
    pub container: String,
    pub local_workspace_dir: String,
    pub db_path: String,
}

impl Config {
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/david".into());
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            host_alias: std::env::var("HOST_ALIAS").unwrap_or_else(|_| "helmor-taper-arm64".into()),
            remote_binary: std::env::var("REMOTE_BINARY")
                .unwrap_or_else(|_| "/home/e2e/.helmor/server/helmor-server".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
            local_workspace_dir: std::env::var("LOCAL_WS_DIR")
                .unwrap_or_else(|_| "/Users/david/helmor-dev/workspaces/helmor-taper/hamal".into()),
            db_path: std::env::var("HELMOR_DB")
                .unwrap_or_else(|_| format!("{home}/helmor-dev/helmor.db")),
        }
    }
}

const PROMPT_LS: &str = "List the files in this workspace, one per line, no preamble.";
const PROMPT_README: &str = "Read README.md from this workspace and quote its second line verbatim. Respond with only that line, no preamble.";
const NEW_FILE: &str = "HELMOR_DEMO.md";
const NEW_FILE_TEXT: &str = "Hello from the remote container";
const MARKER: &str = "REMOTE_ONLY_MARKER.txt";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RuntimeEntry {
    name: String,
    state: Option<RuntimeState>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RuntimeState {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceBinding {
    workspace_id: String,
    runtime_name: String,
    remote_path: String,
}

fn run_cmd(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog).args(args).output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "{prog} {} → exit {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn db_contains(db_path: &str, workspace_id: &str, needle: &str) -> Result<bool> {
    let escaped = needle.replace('\'', "''");
    let sql = format!(
        "SELECT 1 FROM session_messages \
         WHERE session_id IN (SELECT id FROM sessions WHERE workspace_id='{workspace_id}') \
         AND content LIKE '%{escaped}%' LIMIT 1;"
    );
    Ok(!run_cmd("sqlite3", &[db_path, &sql])?.is_empty())
}

async fn send_and_wait(
    tape: &mut Tape,
    prompt: &str,
    label: &str,
    timeout: Duration,
) -> Result<Option<String>> {
    let baseline: u64 = tape
        .js(r#"return document.querySelectorAll('[data-message-role]').length;"#)
        .await?;
    let fire = format!(
        r#"
        (function(){{
            window.__taperLastErr = null;
            window.__helmorTest.sendPrompt({p})
                .catch(function(e){{ window.__taperLastErr = String(e && e.message ? e.message : e); }});
            return "fired";
        }})()"#,
        p = serde_json::to_string(prompt)?,
    );
    tape.js::<Value>(&fire).await?;
    tape.log(&format!("[{label}] sent"));

    let snap_script = format!(
        r#"
        (function(){{
            var msgs = document.querySelectorAll('[data-message-role]');
            var since = msgs.length > {baseline} ? Array.from(msgs).slice({baseline}) : [];
            return {{
                count: msgs.length,
                streaming: !!document.querySelector('[data-testid=streaming-footer]'),
                err: window.__taperLastErr || null,
                panelText: since.map(function(m){{ return m.innerText || ''; }}).join('\n'),
            }};
        }})()"#,
    );

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Snap {
        count: u64,
        streaming: bool,
        err: Option<String>,
        panel_text: String,
    }

    let deadline = Instant::now() + timeout;
    let mut final_text: Option<String> = None;
    while Instant::now() < deadline {
        let snap: Snap = tape.js(&snap_script).await?;
        if let Some(err) = snap.err {
            tape.log(&format!("[{label}] sendPrompt error: {err}"));
            return Ok(None);
        }
        if snap.count > baseline && !snap.streaming && !snap.panel_text.trim().is_empty() {
            final_text = Some(snap.panel_text);
            break;
        }
        tape.sleep(Duration::from_millis(500)).await;
    }
    let preview: String = final_text
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    tape.assert(
        format!("{label}_arrived"),
        final_text.is_some(),
        preview.chars().take(120).collect::<String>(),
    );
    Ok(final_text)
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    // 0. Verify runtime connected.
    {
        let rts: Vec<RuntimeEntry> = tape.invoke("list_remote_runtimes", json!({})).await?;
        let connected = rts
            .iter()
            .find(|r| r.name == config.runtime_name)
            .and_then(|r| r.state.as_ref())
            .and_then(|s| s.kind.as_deref())
            .is_some_and(|k| k == "connected");
        if !connected {
            tape.log("runtime not connected; reconnecting");
            let _ = tape
                .invoke::<Value>(
                    "connect_remote_runtime",
                    json!({
                        "name": config.runtime_name,
                        "host": config.host_alias,
                        "remoteBinary": config.remote_binary,
                        "forwardAgent": false,
                    }),
                )
                .await;
        }
    }
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

    let bindings: Vec<WorkspaceBinding> = tape
        .invoke("list_workspace_runtime_bindings", json!({}))
        .await?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow!("no workspace bound to {}", config.runtime_name))?;

    // Plant marker + clear stale demo file.
    let marker_text = format!("remote-proof-{}", std::process::id());
    let plant_cmd = format!(
        "printf '%s' '{marker_text}' > '{rp}/{MARKER}' && rm -f '{rp}/{NEW_FILE}'",
        rp = bound.remote_path,
    );
    run_cmd(
        "docker",
        &[
            "exec",
            "-u",
            "e2e",
            &config.container,
            "sh",
            "-c",
            &plant_cmd,
        ],
    )?;
    tape.log(&format!("planted {MARKER}; cleared any stale {NEW_FILE}"));

    // Wipe session history + pin LM Studio model.
    let wipe_sql = format!(
        "DELETE FROM session_messages WHERE session_id IN (SELECT id FROM sessions WHERE workspace_id='{}'); \
         UPDATE sessions SET model='claude-custom|custom|google/gemma-4-26b-a4b' WHERE workspace_id='{}';",
        bound.workspace_id, bound.workspace_id
    );
    run_cmd("sqlite3", &[&config.db_path, &wipe_sql])?;
    tape.log(&format!(
        "wiped session_messages + pinned LM Studio model for workspace {}",
        &bound.workspace_id[..bound.workspace_id.len().min(8)]
    ));

    // Reload + select bound workspace + wait for composer hook.
    tape.js::<Value>(r#"window.location.reload(); return "r";"#)
        .await?;
    tape.sleep(Duration::from_secs(6)).await;
    let click_script = format!(
        r#"var el=document.querySelector({body_sel})||document.querySelector({row_sel});
           if (el) el.click(); return !!el;"#,
        body_sel = serde_json::to_string(&format!(
            r#"[data-workspace-row-id="{}"] [data-workspace-row-body]"#,
            bound.workspace_id
        ))?,
        row_sel = serde_json::to_string(&format!(
            r#"[data-workspace-row-id="{}"]"#,
            bound.workspace_id
        ))?,
    );
    tape.js::<bool>(&click_script).await?;
    let chip_visible = tape
        .wait_for(
            r#"[aria-label^="Workspace runtime:"]"#,
            Duration::from_secs(10),
        )
        .await?;
    tape.assert("workspace_runtime_chip", chip_visible, "");

    let mut hook_ready = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        hook_ready = tape
            .js::<bool>(r#"return typeof window.__helmorTest?.sendPrompt === "function";"#)
            .await?;
        if hook_ready {
            break;
        }
        tape.sleep(Duration::from_millis(400)).await;
    }
    tape.assert("composer_hook_attached", hook_ready, "");

    tape.start_recording(140, 6, 900).await?;
    tape.scene(
        SceneSpec::new(format!(
            "Workspace bound to {} — runtime chip in the header. The composer is the same one you'd use locally.",
            config.runtime_name
        ))
        .hold_sec(4),
    )
    .await?;

    // Beat 2: ls.
    send_and_wait(tape, PROMPT_LS, "ls", Duration::from_secs(90)).await?;
    let saw_marker = db_contains(&config.db_path, &bound.workspace_id, MARKER)?;
    tape.assert(
        "ls_tool_result_persisted_marker",
        saw_marker,
        if saw_marker { "yes" } else { "no" },
    );
    tape.scene(
        SceneSpec::new(if saw_marker {
            format!(
                "\"list the files\" → agent ran `ls -1` on the container; {MARKER} came back in the tool result."
            )
        } else {
            "\"list the files\" → reply streamed back from the container.".into()
        })
        .hold_sec(8),
    )
    .await?;

    // Beat 3: README second line.
    send_and_wait(tape, PROMPT_README, "readme", Duration::from_secs(90)).await?;
    let saw_taper = db_contains(&config.db_path, &bound.workspace_id, "helmor-taper")?;
    tape.assert(
        "readme_read_from_container",
        saw_taper,
        if saw_taper { "yes" } else { "no" },
    );
    tape.scene(
        SceneSpec::new(
            "\"read README.md\" → file read from /home/e2e/helmor-workspaces/helmor-taper, line quoted back.",
        )
        .hold_sec(8),
    )
    .await?;

    // Beat 4: create file.
    let create_prompt = format!(
        "Create a file called {NEW_FILE} in this workspace containing the single line: {NEW_FILE_TEXT}"
    );
    send_and_wait(tape, &create_prompt, "create", Duration::from_secs(120)).await?;

    let check_cmd = format!(
        "test -f '{rp}/{NEW_FILE}' && cat '{rp}/{NEW_FILE}' || echo MISSING",
        rp = bound.remote_path,
    );
    let body = run_cmd(
        "docker",
        &[
            "exec",
            "-u",
            "e2e",
            &config.container,
            "sh",
            "-c",
            &check_cmd,
        ],
    )?;
    let exists = body == NEW_FILE_TEXT;
    tape.assert(
        "new_file_on_container",
        exists,
        body.chars().take(120).collect::<String>(),
    );
    tape.scene(
        SceneSpec::new(if exists {
            format!("\"create {NEW_FILE}\" → file lives on the container's disk, not the laptop.")
        } else {
            format!("\"create {NEW_FILE}\" → see inspector for the file write.")
        })
        .hold_sec(8),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "workspaceId": bound.workspace_id,
        "remotePath": bound.remote_path,
        "prompts": {
            "ls": PROMPT_LS,
            "readme": PROMPT_README,
            "create": create_prompt,
        },
        "createdFile": {
            "path": format!("{}/{}", bound.remote_path, NEW_FILE),
            "body": body,
        },
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
            std::env::remove_var("HOST_ALIAS");
            std::env::remove_var("CONTAINER");
            std::env::remove_var("LOCAL_WS_DIR");
            std::env::remove_var("HELMOR_DB");
            std::env::set_var("HOME", "/Users/test");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.container, "helmor-test-linux-arm64");
        assert_eq!(c.db_path, "/Users/test/helmor-dev/helmor.db");
    }

    #[test]
    fn workspace_binding_deserializes_with_remote_path() {
        let v = json!([{
            "workspaceId": "ws-abc",
            "runtimeName": "docker-linux-arm64",
            "remotePath": "/home/e2e/helmor-workspaces/helmor-taper/hamal",
        }]);
        let parsed: Vec<WorkspaceBinding> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed[0].workspace_id, "ws-abc");
        assert!(parsed[0].remote_path.contains("hamal"));
    }
}
