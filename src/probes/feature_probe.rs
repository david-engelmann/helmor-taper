//! Command-level confirmation of the remote-runner feature surface
//! against a live Helmor desktop. Each check invokes the same backend
//! command the UI uses and asserts on the result — a green run proves
//! the feature works end-to-end (desktop → SSH → daemon → container)
//! without recording.
//!
//! Rust port of `scripts/feature-probe.ts`. Writes a JSON report to
//! `feature-probe-report.json` (override with `FEATURE_PROBE_OUT`).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::bridge::Bridge;
use crate::commands::invoke_and_wait;
use crate::probes::run_cmd;

#[derive(Debug, Clone)]
pub struct Config {
    pub host_alias: String,
    pub runtime_name: String,
    pub container: String,
    pub local_workspace_dir: String,
    pub report_path: PathBuf,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            host_alias: std::env::var("HOST_ALIAS").unwrap_or_else(|_| "helmor-taper-arm64".into()),
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
            local_workspace_dir: std::env::var("LOCAL_WS_DIR").unwrap_or_else(|_| {
                "/Users/david/helmor-dev/workspaces/helmor-taper/albiorix".into()
            }),
            report_path: PathBuf::from(
                std::env::var("FEATURE_PROBE_OUT")
                    .unwrap_or_else(|_| "./feature-probe-report.json".into()),
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub feature: String,
    pub track: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceBinding {
    workspace_id: String,
    runtime_name: String,
    remote_path: String,
}

async fn inv(bridge: &Bridge, cmd: &str, args: Value) -> Result<Value> {
    invoke_and_wait(bridge, cmd, args, Duration::from_secs(60), cmd).await
}

fn record(results: &mut Vec<Check>, feature: &str, track: &str, outcome: Result<String>) {
    match outcome {
        Ok(detail) => {
            eprintln!("✓ [{track}] {feature} — {detail}");
            results.push(Check {
                feature: feature.into(),
                track: track.into(),
                ok: true,
                detail,
            });
        }
        Err(err) => {
            let msg = err.to_string().chars().take(160).collect::<String>();
            eprintln!("✗ [{track}] {feature} — {msg}");
            results.push(Check {
                feature: feature.into(),
                track: track.into(),
                ok: false,
                detail: msg,
            });
        }
    }
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let mut results: Vec<Check> = Vec::new();

    // Discover bound workspace.
    let bindings: Vec<WorkspaceBinding> =
        serde_json::from_value(inv(bridge, "list_workspace_runtime_bindings", json!({})).await?)?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .cloned();

    // ── Track B ───────────────────────────────────────────────────
    let res = async {
        let hosts: Vec<String> =
            serde_json::from_value(inv(bridge, "list_ssh_hosts", json!({})).await?)?;
        if !hosts.contains(&config.host_alias) {
            return Err(anyhow!(
                "host {} not in {} hosts",
                config.host_alias,
                hosts.len()
            ));
        }
        Ok(format!("{} hosts incl. {}", hosts.len(), config.host_alias))
    }
    .await;
    record(
        &mut results,
        "SSH host autocomplete (~/.ssh/config)",
        "B",
        res,
    );

    let res = async {
        let details: Vec<Value> =
            serde_json::from_value(inv(bridge, "list_ssh_host_details", json!({})).await?)?;
        let d = details
            .iter()
            .find(|d| d.get("alias").and_then(Value::as_str) == Some(&config.host_alias))
            .ok_or_else(|| anyhow!("no details for {}", config.host_alias))?;
        Ok(format!(
            "{} → {}:{}",
            config.host_alias,
            d.get("hostName").and_then(Value::as_str).unwrap_or(""),
            d.get("port").and_then(Value::as_u64).unwrap_or(22)
        ))
    }
    .await;
    record(
        &mut results,
        "SSH host details (hostname/port/identity)",
        "B",
        res,
    );

    let res = async {
        let ids: Vec<Value> =
            serde_json::from_value(inv(bridge, "list_ssh_identities", json!({})).await?)?;
        Ok(format!("{} identities", ids.len()))
    }
    .await;
    record(&mut results, "SSH identities visibility", "B", res);

    let res = async {
        let s = inv(bridge, "ssh_agent_status", json!({})).await?;
        Ok(serde_json::to_string(&s)?)
    }
    .await;
    record(&mut results, "SSH agent status", "B", res);

    let res = async {
        let p = invoke_and_wait(
            bridge,
            "probe_ssh_host",
            json!({"host": config.host_alias}),
            Duration::from_secs(30),
            "probe-ssh",
        )
        .await?;
        Ok(serde_json::to_string(&p)?
            .chars()
            .take(120)
            .collect::<String>())
    }
    .await;
    record(&mut results, "Pre-connect SSH probe", "B", res);

    // ── Connect + state ───────────────────────────────────────────
    let res = async {
        let rts: Vec<Value> =
            serde_json::from_value(inv(bridge, "list_remote_runtimes", json!({})).await?)?;
        let r = rts
            .iter()
            .find(|r| r.get("name").and_then(Value::as_str) == Some(&config.runtime_name));
        let state = r
            .and_then(|r| r.get("state"))
            .and_then(|s| s.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("(missing)");
        if state != "connected" {
            return Err(anyhow!("state={state}"));
        }
        Ok(format!("{} connected", config.runtime_name))
    }
    .await;
    record(&mut results, "Connected remote runtime", "B/C", res);

    let res = async {
        let h = inv(
            bridge,
            "get_runtime_health",
            json!({"runtimeName": config.runtime_name}),
        )
        .await?;
        let kind = h
            .get("kind")
            .and_then(|k| k.get("type"))
            .and_then(Value::as_str);
        if kind != Some("remote") {
            return Err(anyhow!("expected remote, got {kind:?}"));
        }
        let version = h.get("version").and_then(Value::as_str).unwrap_or("");
        if !version
            .split('.')
            .take(3)
            .all(|p| !p.is_empty() && p.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            return Err(anyhow!("bad version: {version}"));
        }
        let hostname = h.get("hostname").and_then(Value::as_str).unwrap_or("");
        Ok(format!("v{version} on {hostname} (remote)"))
    }
    .await;
    record(&mut results, "Runtime health (host/version)", "B/C", res);

    // ── Track E ───────────────────────────────────────────────────
    let res = async {
        let r = inv(
            bridge,
            "tail_remote_daemon_log",
            json!({"name": config.runtime_name, "maxLines": 20}),
        )
        .await?;
        let lines = r
            .get("lines")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("no lines field"))?;
        Ok(format!("{} log lines", lines.len()))
    }
    .await;
    record(&mut results, "Daemon log tail (E1)", "E", res);

    let res = async {
        let m: Value = inv(
            bridge,
            "get_remote_runtime_metrics",
            json!({"name": config.runtime_name}),
        )
        .await?;
        let keys: Vec<&str> = m
            .as_object()
            .map(|o| o.keys().take(6).map(|s| s.as_str()).collect())
            .unwrap_or_default();
        Ok(format!("metrics keys: {}", keys.join(",")))
    }
    .await;
    record(&mut results, "Per-method RPC metrics (E2)", "E", res);

    let res = async {
        let d: Value = inv(
            bridge,
            "get_remote_runtime_diagnostics",
            json!({"name": config.runtime_name}),
        )
        .await?;
        let keys: Vec<&str> = d
            .as_object()
            .map(|o| o.keys().take(6).map(|s| s.as_str()).collect())
            .unwrap_or_default();
        Ok(format!("diagnostics keys: {}", keys.join(",")))
    }
    .await;
    record(&mut results, "Copy-diagnostics bundle (E3)", "E", res);

    let res = async {
        let a = inv(
            bridge,
            "get_remote_runtime_auth_status",
            json!({"name": config.runtime_name}),
        )
        .await?;
        Ok(serde_json::to_string(&a)?
            .chars()
            .take(120)
            .collect::<String>())
    }
    .await;
    record(&mut results, "Agent auth status (G2)", "G", res);

    // ── Track F2 + core ────────────────────────────────────────────
    record(
        &mut results,
        "Workspace bound to remote (F2/B5)",
        "F",
        if let Some(b) = bound.as_ref() {
            Ok(format!(
                "{}… → {} @ {}",
                &b.workspace_id[..b.workspace_id.len().min(8)],
                config.runtime_name,
                b.remote_path
            ))
        } else {
            Err(anyhow!("no workspace bound to remote"))
        },
    );

    if let Some(b) = bound.as_ref() {
        let res = async {
            let p: Option<String> = serde_json::from_value(
                inv(
                    bridge,
                    "get_remembered_workspace_remote_path",
                    json!({"workspaceId": b.workspace_id, "runtimeName": config.runtime_name}),
                )
                .await?,
            )?;
            if p.as_deref() != Some(&b.remote_path) {
                return Err(anyhow!("remembered={p:?}"));
            }
            Ok(format!("remembered {}", b.remote_path))
        }
        .await;
        record(&mut results, "Per-host remote path memory (F2.1)", "F", res);
    }

    // Plant marker on container.
    const MARKER: &str = "REMOTE_ONLY_MARKER.txt";
    let marker_text = format!("remote-proof-{}", std::process::id());
    if let Some(b) = bound.as_ref() {
        let plant_cmd = format!(
            "printf '%s' '{marker_text}' > '{rp}/{MARKER}'",
            rp = b.remote_path
        );
        let _ = run_cmd(
            "docker",
            &["exec", &config.container, "sh", "-c", &plant_cmd],
        );
    }

    if let Some(b) = bound.as_ref() {
        let ws_id = b.workspace_id.clone();
        let res = async {
            let s = inv(
                bridge,
                "get_workspace_status",
                json!({"workspaceDir": config.local_workspace_dir, "workspaceId": ws_id}),
            )
            .await?;
            let body = serde_json::to_string(&s)?;
            if !body.contains(MARKER) {
                return Err(anyhow!(
                    "remote marker not in status: {}",
                    body.chars().take(120).collect::<String>()
                ));
            }
            Ok(format!("status from remote sees {MARKER}"))
        }
        .await;
        record(&mut results, "Remote git status", "core", res);

        let ws_id = b.workspace_id.clone();
        let res = async {
            let s = inv(
                bridge,
                "get_workspace_branch_info",
                json!({"workspaceDir": config.local_workspace_dir, "workspaceId": ws_id}),
            )
            .await?;
            Ok(format!(
                "branch: {}",
                serde_json::to_string(&s)?
                    .chars()
                    .take(90)
                    .collect::<String>()
            ))
        }
        .await;
        record(&mut results, "Remote branch info", "core", res);

        let ws_id = b.workspace_id.clone();
        let res = async {
            let r = inv(
                bridge,
                "read_workspace_file",
                json!({
                    "workspaceDir": config.local_workspace_dir,
                    "relativePath": MARKER,
                    "workspaceId": ws_id,
                }),
            )
            .await?;
            let content = r.get("content").and_then(Value::as_str).unwrap_or("");
            if content != marker_text {
                return Err(anyhow!(
                    "got: {}",
                    content.chars().take(60).collect::<String>()
                ));
            }
            Ok("read remote-only marker (content matches)".into())
        }
        .await;
        record(
            &mut results,
            "Remote file read (content from container)",
            "core",
            res,
        );

        let ws_id = b.workspace_id.clone();
        let res = async {
            let r = inv(
                bridge,
                "get_workspace_file_tree",
                json!({"workspaceDir": config.local_workspace_dir, "workspaceId": ws_id}),
            )
            .await?;
            let entries = r
                .get("entries")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = entries.len();
            let saw = entries.iter().any(|e| {
                let name = e.get("name").and_then(Value::as_str).unwrap_or("");
                let path = e.get("path").and_then(Value::as_str).unwrap_or("");
                name.contains(MARKER) || path.contains(MARKER)
            });
            if !saw {
                return Err(anyhow!("marker not in {n}-entry tree"));
            }
            Ok(format!("{n} entries from remote (incl. marker)"))
        }
        .await;
        record(&mut results, "Remote file tree", "core", res);

        let ws_id = b.workspace_id.clone();
        let res = async {
            let r = inv(
                bridge,
                "search_workspace",
                json!({
                    "workspaceDir": config.local_workspace_dir,
                    "query": "Helmor",
                    "maxResults": 5,
                    "caseInsensitive": true,
                    "workspaceId": ws_id,
                }),
            )
            .await?;
            let matches = r
                .get("matches")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if matches.is_empty() {
                return Err(anyhow!("no matches on remote (README should match)"));
            }
            let first = matches[0]
                .get("relativePath")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(format!("{} matches from remote ({first})", matches.len()))
        }
        .await;
        record(
            &mut results,
            "Remote workspace search (git grep)",
            "core",
            res,
        );

        let ws_id = b.workspace_id.clone();
        let res = async {
            let r = inv(
                bridge,
                "read_workspace_file_at_ref",
                json!({
                    "workspaceDir": config.local_workspace_dir,
                    "relativePath": "README.md",
                    "gitRef": "HEAD",
                    "workspaceId": ws_id,
                }),
            )
            .await?;
            let content = r
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            if !content.contains("helmor-taper") {
                return Err(anyhow!(
                    "unexpected: {}",
                    content.chars().take(50).collect::<String>()
                ));
            }
            Ok("README.md@HEAD read from remote".into())
        }
        .await;
        record(
            &mut results,
            "Remote file read at git ref (diff base)",
            "core",
            res,
        );
    }

    let passed = results.iter().filter(|r| r.ok).count();
    let total = results.len();
    eprintln!("\n{passed}/{total} feature checks passed");

    let report = json!({
        "host": config.host_alias,
        "runtime": config.runtime_name,
        "bound": bound.as_ref().map(|b| json!({
            "workspaceId": b.workspace_id,
            "remotePath": b.remote_path,
        })),
        "passed": passed,
        "total": total,
        "results": results,
    });
    std::fs::write(&config.report_path, serde_json::to_string_pretty(&report)?)?;

    Ok(passed == total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("HOST_ALIAS");
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("CONTAINER");
            std::env::remove_var("LOCAL_WS_DIR");
            std::env::remove_var("FEATURE_PROBE_OUT");
        }
        let c = Config::from_env();
        assert_eq!(c.host_alias, "helmor-taper-arm64");
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.report_path, PathBuf::from("./feature-probe-report.json"));
    }

    #[test]
    fn check_serializes_with_camel_case_fields_via_json_value() {
        let c = Check {
            feature: "f".into(),
            track: "T".into(),
            ok: true,
            detail: "d".into(),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["feature"], "f");
        assert_eq!(v["ok"], true);
    }
}
