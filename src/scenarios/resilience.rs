//! Track C proof: when the remote host disappears (modeled by
//! `docker stop` of the container hosting helmor-server) Helmor's
//! liveness loop notices, flips the runtime to a non-`connected`
//! state, surfaces a top-of-shell banner, and a one-click Reconnect
//! re-establishes SSH once the host comes back.
//!
//! Rust port of `scenarios/resilience.ts`. Shells out to `docker stop`
//! and `docker start` for the chaos events; the rest is plain bridge
//! driving. Idempotent: if the runtime is already disconnected when
//! the scenario starts (the user kicked it before recording), it
//! reconnects first so the failure → recovery story stays meaningful.

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
    pub container: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuntimeEntry {
    pub name: String,
    pub state: Option<RuntimeState>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RuntimeState {
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

fn docker(args: &[&str]) -> Result<String> {
    let out = Command::new("docker").args(args).output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "docker {} → exit {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Poll `list_remote_runtimes` until the named runtime's `state.type`
/// satisfies `predicate`, or `timeout` elapses. Returns the last
/// observed label (which may be "(missing)" or "(unknown)").
async fn wait_for_state<F>(
    tape: &mut Tape,
    runtime_name: &str,
    predicate: F,
    timeout: Duration,
) -> Result<String>
where
    F: Fn(&str) -> bool,
{
    let deadline = Instant::now() + timeout;
    let mut last = "(unknown)".to_string();
    while Instant::now() < deadline {
        let runtimes: Vec<RuntimeEntry> = tape.invoke("list_remote_runtimes", json!({})).await?;
        let entry = runtimes.iter().find(|r| r.name == runtime_name);
        last = entry
            .and_then(|r| r.state.as_ref())
            .and_then(|s| s.kind.clone())
            .unwrap_or_else(|| {
                if entry.is_none() {
                    "(missing)".into()
                } else {
                    "(unknown)".into()
                }
            });
        if predicate(&last) {
            return Ok(last);
        }
        tape.sleep(Duration::from_millis(500)).await;
    }
    Ok(last)
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    // 0. Make sure we start connected.
    let initial = wait_for_state(
        tape,
        &config.runtime_name,
        |s| s == "connected",
        Duration::from_secs(5),
    )
    .await?;
    if initial != "connected" {
        tape.log(&format!(
            "runtime {} state={initial}; reconnecting first",
            config.runtime_name
        ));
        tape.invoke::<Value>(
            "reconnect_remote_runtime",
            json!({"name": config.runtime_name}),
        )
        .await?;
        wait_for_state(
            tape,
            &config.runtime_name,
            |s| s == "connected",
            Duration::from_secs(30),
        )
        .await?;
    }

    tape.js::<Value>(r#"window.location.reload(); return "r";"#)
        .await?;
    tape.sleep(Duration::from_secs(6)).await;
    tape.open_settings("remote-servers").await?;
    let panel_opens = tape
        .wait_for("[role=dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("panel_opens", panel_opens, "");
    let row_selector = format!("[data-testid=remote-server-row-{}]", config.runtime_name);
    let row_present = tape
        .wait_for(&row_selector, Duration::from_secs(10))
        .await?;
    tape.assert("row_present", row_present, "");

    // Baseline.
    tape.scene(
        SceneSpec::new(format!(
            "Baseline: {} is connected — helmor-server is alive on the container",
            config.runtime_name
        ))
        .hold_sec(4),
    )
    .await?;

    // Kill the host.
    tape.close_dialog().await?;
    tape.sleep(Duration::from_millis(400)).await;
    tape.log(&format!("stopping container {}", config.container));
    docker(&["stop", "-t", "1", &config.container])?;

    let down_state = wait_for_state(
        tape,
        &config.runtime_name,
        |s| s != "connected",
        Duration::from_secs(20),
    )
    .await?;
    tape.assert(
        "state_flips_offline",
        down_state != "connected",
        format!("state={down_state}"),
    );
    let banner_selector = format!(
        "[data-testid=remote-connection-banner-row-{}]",
        config.runtime_name
    );
    let banner_appears = tape
        .wait_for(&banner_selector, Duration::from_secs(10))
        .await?;
    tape.assert("banner_appears", banner_appears, "top-of-shell banner");
    tape.scene(
        SceneSpec::new(format!(
            "docker stop {} → liveness ping fails → banner flips to \"{down_state}\"",
            config.container
        ))
        .record_sec(3)
        .hold_sec(6),
    )
    .await?;

    // Bring it back + reconnect.
    tape.log(&format!("starting container {}", config.container));
    docker(&["start", &config.container])?;
    tape.sleep(Duration::from_millis(3500)).await;

    let reconnect_script = format!(
        r#"var r=document.querySelector({sel}); if(r){{r.click(); return true;}} return false;"#,
        sel = serde_json::to_string(&format!(
            "[data-testid=remote-connection-banner-row-{}] button",
            config.runtime_name
        ))?,
    );
    let clicked: bool = tape.js(&reconnect_script).await?;
    if clicked {
        tape.log("clicked Reconnect button in banner");
    } else {
        tape.log("no banner button; falling back to reconnect_remote_runtime");
        tape.invoke::<Value>(
            "reconnect_remote_runtime",
            json!({"name": config.runtime_name}),
        )
        .await?;
    }

    let recovered = wait_for_state(
        tape,
        &config.runtime_name,
        |s| s == "connected",
        Duration::from_secs(60),
    )
    .await?;
    tape.assert(
        "state_recovers",
        recovered == "connected",
        format!("state={recovered}"),
    );

    tape.open_settings("remote-servers").await?;
    tape.wait_for(&row_selector, Duration::from_secs(10))
        .await?;
    tape.sleep(Duration::from_millis(800)).await;

    tape.scene(
        SceneSpec::new(format!(
            "Reconnect → SSH re-establishes → {} green again. No restart, no losing your work.",
            config.runtime_name
        ))
        .hold_sec(6),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "downState": down_state,
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
            std::env::remove_var("CONTAINER");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.container, "helmor-test-linux-arm64");
    }

    #[test]
    fn docker_propagates_non_zero_exit_with_stderr() {
        let err = docker(&["nonexistent-subcommand-xyz"]).expect_err("must fail");
        let msg = err.to_string();
        // Errors should mention either non-zero exit OR the docker not-installed case.
        assert!(
            msg.contains("docker") || msg.contains("No such file"),
            "expected docker-related error: {msg}"
        );
    }
}
