//! THE demo. One scenario, one `master.gif`, walks a reviewer through
//! the full user journey for the remote-runner feature in 75–90s.
//!
//! Rust port of `scenarios/end-to-end-demo.ts`. Composes patterns
//! from every other scenario in sequence — install-chip transitions,
//! workspace binding, inspector probe, composer-driven chat, docker
//! stop/start chaos, reconnect — into one master tape.
//!
//! Beats (each `tape.scene` call advances the recording timeline):
//! Beat 1: Remote Servers panel — connected runtime baseline.
//! Beats 2-4: Reinstall click → install chip transitions through
//! detecting / uploading / installed.
//! Beat 5: Workspace bound to remote — header chip live.
//! Beats 6-7: Inspector probe → file tree + Run changes (marker visible).
//! Beat 8a: Chat: list files → marker in tool_result.
//! Beat 8b: Chat: hostname → container hostname in tool_result.
//! Beat 9: "All green."
//! Beat 10: docker stop → banner flips Degraded.
//! Beat 11: docker start → Reconnect → green.
//! Beat 13: Close on the Remote Servers panel ("rm -rf $HOME/.helmor/server").

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
            host_alias: std::env::var("HOST_ALIAS")
                .unwrap_or_else(|_| "helmor-taper-arm64".into()),
            remote_binary: std::env::var("REMOTE_BINARY")
                .unwrap_or_else(|_| "/home/e2e/.helmor/server/helmor-server".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
            local_workspace_dir: std::env::var("LOCAL_WS_DIR").unwrap_or_else(|_| {
                "/Users/david/helmor-dev/workspaces/helmor-taper/aludra".into()
            }),
            db_path: std::env::var("HELMOR_DB")
                .unwrap_or_else(|_| format!("{home}/helmor-dev/helmor.db")),
        }
    }
}

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
    Ok(final_text)
}

async fn wait_for_state<F>(
    tape: &mut Tape,
    runtime_name: &str,
    predicate: F,
    timeout: Duration,
) -> Result<()>
where
    F: Fn(&str) -> bool,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let rts: Vec<RuntimeEntry> = tape.invoke("list_remote_runtimes", json!({})).await?;
        let label = rts
            .iter()
            .find(|r| r.name == runtime_name)
            .and_then(|r| r.state.as_ref())
            .and_then(|s| s.kind.as_deref())
            .unwrap_or("(missing)");
        if predicate(label) {
            return Ok(());
        }
        tape.sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    // ── Preconditions ─────────────────────────────────────────────
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

    let bindings: Vec<WorkspaceBinding> = tape
        .invoke("list_workspace_runtime_bindings", json!({}))
        .await?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow!("no workspace bound to {}", config.runtime_name))?;
    tape.log(&format!(
        "bound workspace: {}… → {}",
        &bound.workspace_id[..bound.workspace_id.len().min(8)],
        bound.remote_path
    ));

    // Wipe container bundle (beat 3 needs real upload).
    let wipe_bundle = "rm -f $HOME/.helmor/server/helmor-sidecar; \
                       rm -f $HOME/.helmor/server/claude; \
                       rm -f $HOME/.helmor/server/MANIFEST.json; \
                       rm -rf $HOME/.helmor/server/.staging; \
                       if [ -f $HOME/.helmor/server/helmor-server.real ]; then \
                         mv -f $HOME/.helmor/server/helmor-server.real $HOME/.helmor/server/helmor-server; \
                       fi";
    run_cmd(
        "docker",
        &["exec", "-u", "e2e", &config.container, "sh", "-c", wipe_bundle],
    )?;
    tape.log("wiped container bundle artifacts");

    // Plant marker.
    let marker_text = format!("remote-proof-{}", std::process::id());
    let plant_cmd = format!(
        "printf '%s' '{marker_text}' > '{rp}/{MARKER}'",
        rp = bound.remote_path,
    );
    run_cmd(
        "docker",
        &["exec", "-u", "e2e", &config.container, "sh", "-c", &plant_cmd],
    )?;
    tape.log(&format!("planted {MARKER} on container"));

    // Configure LM Studio bridge.
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

    // Wipe session history.
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

    // Reload + open Remote Servers panel.
    tape.js::<Value>(r#"window.location.reload(); return "r";"#).await?;
    tape.sleep(Duration::from_secs(6)).await;
    tape.open_settings("remote-servers").await?;
    let panel_open = tape
        .wait_for("[role=dialog]", Duration::from_secs(10))
        .await?;
    tape.assert("panel_opens", panel_open, "");
    let row_selector = format!("[data-testid=remote-server-row-{}]", config.runtime_name);
    let row_present = tape
        .wait_for(&row_selector, Duration::from_secs(10))
        .await?;
    tape.assert("row_present", row_present, "");

    // Start recording (140 s = ~8 beats + headroom).
    tape.start_recording(140, 8, 960).await?;

    // ── Beat 1 — connected baseline ───────────────────────────────
    tape.scene(
        SceneSpec::new(format!(
            "Helmor — connected to {}, no agent runtime yet",
            config.runtime_name
        ))
        .hold_sec(5),
    )
    .await?;

    // ── Beats 2-4 — install chip transitions ──────────────────────
    let reinstall_sel = format!(
        "[data-testid=remote-server-reinstall-bundle-{}]",
        config.runtime_name
    );
    tape.click(&reinstall_sel).await?;
    let installing_sel = format!(
        "[data-testid=remote-server-bundle-installing-{}]",
        config.runtime_name
    );
    let installing = tape
        .wait_for(&installing_sel, Duration::from_secs(10))
        .await?;
    tape.assert("installing_chip", installing, "");
    tape.scene(
        SceneSpec::new("Reinstall → sha256-verified tar-stream over SSH, atomic per-file")
            .record_sec(3)
            .hold_sec(5),
    )
    .await?;

    tape.sleep(Duration::from_secs(2)).await;
    tape.scene(
        SceneSpec::new("Everything lands in $HOME/.helmor/server/ — no sudo, no shell rc edits")
            .record_sec(3)
            .hold_sec(5),
    )
    .await?;

    let installed_sel = format!(
        "[data-testid=remote-server-bundle-installed-{}]",
        config.runtime_name
    );
    let installed = tape
        .wait_for(&installed_sel, Duration::from_secs(60))
        .await?;
    tape.assert("installed_chip", installed, "");
    let chip_script = format!(
        r#"var c=document.querySelector({sel}); return c?c.innerText:null;"#,
        sel = serde_json::to_string(&installed_sel)?,
    );
    let chip_text: Option<String> = tape.js(&chip_script).await?;
    tape.log(&format!(
        "install chip: {}",
        chip_text.clone().unwrap_or_default()
    ));
    tape.scene(
        SceneSpec::new(
            chip_text
                .clone()
                .map(|t| format!("{t} · ready to run agents on the container"))
                .unwrap_or_else(|| "Agent runtime installed".into()),
        )
        .hold_sec(5),
    )
    .await?;

    // ── Beat 5 — workspace bound to remote ────────────────────────
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(500)).await;
    let click_workspace = format!(
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
    tape.js::<bool>(&click_workspace).await?;
    tape.wait_for(
        r#"[aria-label^="Workspace runtime:"]"#,
        Duration::from_secs(10),
    )
    .await?;
    tape.scene(
        SceneSpec::new(format!(
            "Workspace bound to {} — the blue chip says \"files live in the container\"",
            config.runtime_name
        ))
        .hold_sec(5),
    )
    .await?;

    // ── Beats 6-7 — inspector probe (file tree + changes) ─────────
    tape.open_settings("runtime-debug").await?;
    let debug_panel = tape
        .wait_for("[role=dialog]", Duration::from_secs(10))
        .await?;
    tape.assert("debug_panel_opens", debug_panel, "");
    tape.scroll_to_section("#inspector-probe-workspace").await?;
    tape.sleep(Duration::from_millis(400)).await;
    tape.set_input_value("#inspector-probe-workspace-id", &bound.workspace_id)
        .await?;
    tape.set_input_value("#inspector-probe-workspace", &config.local_workspace_dir)
        .await?;
    tape.sleep(Duration::from_millis(300)).await;
    tape.click_button_by_text("Run file tree").await?;
    tape.wait_for_text(
        "[role=dialog]",
        "files (showing first",
        Duration::from_secs(10),
    )
    .await?;
    tape.scene(
        SceneSpec::new(format!(
            "File tree → entries come from {} on the container",
            bound.remote_path
        ))
        .hold_sec(6),
    )
    .await?;

    tape.click_button_by_text("Run changes").await?;
    tape.wait_for_text("[role=dialog]", MARKER, Duration::from_secs(10))
        .await?;
    tape.scene(
        SceneSpec::new(
            "Planted a file via docker exec → Run changes lists it. Proof: container, not laptop.",
        )
        .hold_sec(6),
    )
    .await?;

    // ── Beats 8a + 8b — composer-driven chat ──────────────────────
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(500)).await;
    let mut hook_ready = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        hook_ready = tape
            .js::<bool>(
                r#"return typeof window.__helmorTest?.sendPrompt === "function";"#,
            )
            .await?;
        if hook_ready {
            break;
        }
        tape.sleep(Duration::from_millis(300)).await;
    }
    tape.assert("composer_hook_attached", hook_ready, "");

    let ls_answer = send_and_wait(
        tape,
        "List the files in this workspace, one per line, no preamble.",
        "chat_ls",
        Duration::from_secs(90),
    )
    .await?;
    tape.assert(
        "chat_ls_arrived",
        ls_answer.is_some(),
        ls_answer
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>(),
    );
    let db_has_marker = db_contains(&config.db_path, &bound.workspace_id, MARKER)?;
    tape.assert(
        "ls_tool_result_persisted_marker",
        db_has_marker,
        if db_has_marker { "yes" } else { "no" },
    );
    tape.scene(
        SceneSpec::new(format!(
            "Chat: \"list the files\" → agent ran `ls -1` inside the container; {MARKER} came back."
        ))
        .hold_sec(8),
    )
    .await?;

    let container_hostname = run_cmd("docker", &["exec", &config.container, "hostname"])?;
    let isolation_answer = send_and_wait(
        tape,
        "Run the shell command `hostname` and reply with only its raw output.",
        "chat_hostname",
        Duration::from_secs(90),
    )
    .await?;
    tape.assert(
        "chat_hostname_arrived",
        isolation_answer.is_some(),
        isolation_answer
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>(),
    );
    let db_has_hostname = db_contains(&config.db_path, &bound.workspace_id, &container_hostname)?;
    tape.assert(
        "hostname_tool_result_is_container",
        db_has_hostname,
        format!("container={container_hostname}, db_has={db_has_hostname}"),
    );
    tape.scene(
        SceneSpec::new(format!(
            "Chat: \"hostname?\" → {container_hostname}. The laptop is just the viewport."
        ))
        .hold_sec(8),
    )
    .await?;

    // ── Beat 9 — all green ────────────────────────────────────────
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(500)).await;
    tape.scene(
        SceneSpec::new("All ops route to the container. Your laptop is just the viewport.")
            .hold_sec(3),
    )
    .await?;

    // ── Beat 10 — docker stop, banner appears ─────────────────────
    tape.log(&format!("stopping container {}", config.container));
    run_cmd("docker", &["stop", "-t", "1", &config.container])?;
    wait_for_state(
        tape,
        &config.runtime_name,
        |s| s != "connected",
        Duration::from_secs(20),
    )
    .await?;
    let banner_sel = format!(
        "[data-testid=remote-connection-banner-row-{}]",
        config.runtime_name
    );
    let banner_visible = tape
        .wait_for(&banner_sel, Duration::from_secs(10))
        .await?;
    tape.assert("banner_visible", banner_visible, "");
    tape.scene(
        SceneSpec::new("docker stop → liveness ping fails → banner flips to Degraded").hold_sec(6),
    )
    .await?;

    // ── Beat 11 — docker start + reconnect ────────────────────────
    tape.log(&format!("starting container {}", config.container));
    run_cmd("docker", &["start", &config.container])?;
    tape.sleep(Duration::from_millis(3500)).await;
    let reconnect_script = format!(
        r#"var r=document.querySelector({sel}); if(r){{r.click(); return true;}} return false;"#,
        sel = serde_json::to_string(&format!(
            "[data-testid=remote-connection-banner-row-{}] button",
            config.runtime_name
        ))?,
    );
    let clicked: bool = tape.js(&reconnect_script).await?;
    if !clicked {
        let _ = tape
            .invoke::<Value>(
                "reconnect_remote_runtime",
                json!({"name": config.runtime_name}),
            )
            .await;
    }
    wait_for_state(
        tape,
        &config.runtime_name,
        |s| s == "connected",
        Duration::from_secs(30),
    )
    .await?;
    tape.scene(
        SceneSpec::new("docker start → Reconnect → green. Same daemon, same workspace, same sessions.")
            .record_sec(3)
            .hold_sec(6),
    )
    .await?;

    // ── Beat 13 — close out ───────────────────────────────────────
    tape.open_settings("remote-servers").await?;
    tape.wait_for(&row_selector, Duration::from_secs(10)).await?;
    tape.scene(
        SceneSpec::new(
            "Everything Helmor wrote is in $HOME/.helmor/server/. Uninstall = rm -rf that one dir.",
        )
        .hold_sec(6),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "workspaceId": bound.workspace_id,
        "remotePath": bound.remote_path,
        "chipText": chip_text,
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
            std::env::remove_var("REMOTE_BINARY");
            std::env::remove_var("CONTAINER");
            std::env::remove_var("LOCAL_WS_DIR");
            std::env::remove_var("HELMOR_DB");
            std::env::set_var("HOME", "/Users/demo");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.host_alias, "helmor-taper-arm64");
        assert_eq!(c.db_path, "/Users/demo/helmor-dev/helmor.db");
    }
}
