//! Headline isolation tape: three back-to-back chat exchanges that
//! prove the agent runs on the container, not the laptop.
//!
//! Rust port of `scenarios/isolation-proof.ts`.
//!
//! Beats:
//! 1. Workspace selected, empty chat (DB pre-wiped).
//! 2. `hostname` → container's randomized hostname (NOT the laptop's).
//! 3. `[ -e /Users/david ] && echo yes || echo no` → "no" (the
//!    container has no /Users tree).
//! 4. `pwd` → `/home/e2e/...` (container path, not a laptop path).
//!
//! Cross-cutting assertion: the laptop's hostname doesn't appear
//! anywhere in the captured session_messages. Combined with the
//! container hostname being present, that's the strongest form of
//! the isolation proof — anything that could come from the laptop's
//! `gh` or local filesystem would fail both halves.

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
    /// SQLite DB path; defaults to `$HOME/helmor-dev/helmor.db`.
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
            db_path: std::env::var("HELMOR_DB")
                .unwrap_or_else(|_| format!("{home}/helmor-dev/helmor.db")),
        }
    }
}

const PROMPT_HOSTNAME: &str =
    "Run the shell command `hostname` and reply with only its raw output.";
const PROMPT_USERS: &str =
    "Run the shell command `[ -e /Users/david ] && echo yes || echo no` and reply with only its output.";
const PROMPT_PWD: &str =
    "Run the shell command `pwd` and reply with only the path it prints.";

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

fn db_contains_recent(
    db_path: &str,
    workspace_id: &str,
    needle: &str,
    since_iso: &str,
) -> Result<bool> {
    let escaped_needle = needle.replace('\'', "''");
    let sql = format!(
        "SELECT 1 FROM session_messages \
         WHERE session_id IN (SELECT id FROM sessions WHERE workspace_id='{workspace_id}') \
         AND created_at > '{since_iso}' \
         AND content LIKE '%{escaped_needle}%' LIMIT 1;"
    );
    let out = run_cmd("sqlite3", &[db_path, &sql])?;
    Ok(!out.is_empty())
}

fn now_iso() -> String {
    // We can't pull in chrono. Use SystemTime + the inline ISO formatter
    // already exported by tape::mod.
    use std::time::{SystemTime, UNIX_EPOCH};
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let hour = (tod / 3600) as u32;
    let minute = ((tod % 3600) / 60) as u32;
    let second = (tod % 60) as u32;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m, d)
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

    let fire_script = format!(
        r#"
        (function(){{
            window.__taperLastErr = null;
            window.__helmorTest.sendPrompt({prompt})
                .catch(function(e){{ window.__taperLastErr = String(e && e.message ? e.message : e); }});
            return "fired";
        }})()"#,
        prompt = serde_json::to_string(prompt)?,
    );
    tape.js::<Value>(&fire_script).await?;
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
    let arrived = final_text.is_some();
    let preview = final_text
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    tape.assert(
        format!("{label}_arrived"),
        arrived,
        preview.chars().take(120).collect::<String>(),
    );
    Ok(final_text)
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    // 0. Verify runtime connected (reconnect if needed).
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

    let bindings: Vec<WorkspaceBinding> = tape
        .invoke("list_workspace_runtime_bindings", json!({}))
        .await?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow!("no workspace bound to {}", config.runtime_name))?;

    let container_hostname = run_cmd("docker", &["exec", &config.container, "hostname"])?;
    let laptop_hostname = run_cmd("hostname", &[])?;
    tape.log(&format!(
        "container hostname={container_hostname}, laptop hostname={laptop_hostname}"
    ));

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

    // Reload + select workspace + wait for composer hook.
    tape.js::<Value>(r#"window.location.reload(); return "r";"#).await?;
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
            .js::<bool>(
                r#"return typeof window.__helmorTest?.sendPrompt === "function";"#,
            )
            .await?;
        if hook_ready {
            break;
        }
        tape.sleep(Duration::from_millis(400)).await;
    }
    tape.assert("composer_hook_attached", hook_ready, "");

    tape.start_recording(120, 6, 900).await?;
    tape.scene(
        SceneSpec::new(
            "The agent below runs in a Docker container; the laptop is just the viewport.",
        )
        .hold_sec(4),
    )
    .await?;

    // Beat 2: hostname.
    let t1 = now_iso();
    send_and_wait(tape, PROMPT_HOSTNAME, "hostname", Duration::from_secs(90)).await?;
    let saw_container_host =
        db_contains_recent(&config.db_path, &bound.workspace_id, &container_hostname, &t1)?;
    let saw_laptop_host = if laptop_hostname.len() > 3 {
        db_contains_recent(&config.db_path, &bound.workspace_id, &laptop_hostname, &t1)?
    } else {
        false
    };
    tape.assert(
        "hostname_is_container_not_laptop",
        saw_container_host && !saw_laptop_host,
        format!(
            "container_seen={saw_container_host}, laptop_seen={saw_laptop_host} (container={container_hostname}, laptop={laptop_hostname})"
        ),
    );
    tape.scene(
        SceneSpec::new(format!(
            "\"hostname\" → {container_hostname} (container). Laptop's hostname doesn't appear anywhere."
        ))
        .hold_sec(8),
    )
    .await?;

    // Beat 3: /Users path absence.
    let t2 = now_iso();
    send_and_wait(tape, PROMPT_USERS, "users_path", Duration::from_secs(90)).await?;
    let saw_no_tool_result =
        db_contains_recent(&config.db_path, &bound.workspace_id, r#""content":"no""#, &t2)?;
    let saw_no_text_block =
        db_contains_recent(&config.db_path, &bound.workspace_id, r#""text":"no""#, &t2)?;
    let saw_no = saw_no_tool_result || saw_no_text_block;
    tape.assert(
        "users_path_reported_absent",
        saw_no,
        if saw_no {
            format!("yes (toolResult={saw_no_tool_result}, textBlock={saw_no_text_block})")
        } else {
            "no".into()
        },
    );
    tape.scene(
        SceneSpec::new(
            "\"/Users/david exist?\" → no. The container's filesystem has no /Users tree at all.",
        )
        .hold_sec(8),
    )
    .await?;

    // Beat 4: pwd on container path.
    let t3 = now_iso();
    send_and_wait(tape, PROMPT_PWD, "pwd", Duration::from_secs(90)).await?;
    let on_container_path =
        db_contains_recent(&config.db_path, &bound.workspace_id, "/home/e2e/", &t3)?;
    tape.assert(
        "pwd_on_container_path",
        on_container_path,
        if on_container_path { "yes" } else { "no" },
    );
    tape.scene(
        SceneSpec::new(
            "\"pwd\" → /home/e2e/... — the agent's CWD lives on the container, not the laptop.",
        )
        .hold_sec(8),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "workspaceId": bound.workspace_id,
        "remotePath": bound.remote_path,
        "hostnames": {
            "container": container_hostname,
            "laptop": laptop_hostname,
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
            std::env::remove_var("REMOTE_BINARY");
            std::env::remove_var("CONTAINER");
            std::env::remove_var("HELMOR_DB");
            std::env::set_var("HOME", "/Users/test");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.host_alias, "helmor-taper-arm64");
        assert_eq!(c.db_path, "/Users/test/helmor-dev/helmor.db");
    }

    #[test]
    fn workspace_binding_deserializes_with_remote_path() {
        let v = json!([{
            "workspaceId": "abc-123",
            "runtimeName": "docker-linux-arm64",
            "remotePath": "/home/e2e/workspace",
        }]);
        let parsed: Vec<WorkspaceBinding> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed[0].workspace_id, "abc-123");
        assert_eq!(parsed[0].remote_path, "/home/e2e/workspace");
    }

    #[test]
    fn now_iso_is_well_formed() {
        let ts = now_iso();
        assert_eq!(ts.len(), 24, "ISO-8601 ms is exactly 24 chars: {ts}");
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
        // Year prefix 20xx for the next 80 years.
        assert!(ts.starts_with("20"));
    }
}
