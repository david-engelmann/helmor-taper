//! Headline demo for the productionized install flow: a fresh host
//! (with helmor-server but no bundle) gets the agent runtime
//! installed automatically on connect, and the Remote Servers row's
//! live chip narrates what's happening end-to-end.
//!
//! Rust port of `scenarios/first-connect-bundle.ts`. The container
//! bundle is wiped via `docker exec` before recording so the install
//! beats are real cold-install transitions, not no-ops.

use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
    pub host_alias: String,
    pub container: String,
    pub remote_binary: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            host_alias: std::env::var("HOST_ALIAS")
                .unwrap_or_else(|_| "helmor-taper-arm64".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
            remote_binary: std::env::var("REMOTE_BINARY")
                .unwrap_or_else(|_| "/home/e2e/.helmor/server/helmor-server".into()),
        }
    }
}

fn wipe_container_bundle(container: &str) -> Result<()> {
    // Mirror of the TS port's `docker exec ... sh -c "rm -f ...; mv -f ..."`.
    let script = "rm -f $HOME/.helmor/server/helmor-sidecar; \
                  rm -f $HOME/.helmor/server/claude; \
                  rm -f $HOME/.helmor/server/MANIFEST.json; \
                  rm -rf $HOME/.helmor/server/.staging; \
                  if [ -f $HOME/.helmor/server/helmor-server.real ]; then \
                    mv -f $HOME/.helmor/server/helmor-server.real $HOME/.helmor/server/helmor-server; \
                  fi";
    let out = Command::new("docker")
        .args(["exec", "-u", "e2e", container, "sh", "-c", script])
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "wipe bundle: docker exec exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    wipe_container_bundle(&config.container)?;
    tape.log("wiped container bundle artifacts");

    tape.js::<Value>(r#"window.location.reload(); return "r";"#).await?;
    tape.sleep(Duration::from_secs(6)).await;
    tape.open_settings("remote-servers").await?;
    let panel_opens = tape
        .wait_for("[role=dialog]", Duration::from_secs(10))
        .await?;
    tape.assert("panel_opens", panel_opens, "");
    let row_selector = format!("[data-testid=remote-server-row-{}]", config.runtime_name);
    let row_present = tape
        .wait_for(&row_selector, Duration::from_secs(10))
        .await?;
    tape.assert("row_present", row_present, "");
    tape.scene(
        SceneSpec::new(format!(
            "Remote Servers panel — {} is connected but has no agent runtime yet",
            config.runtime_name
        ))
        .hold_sec(4),
    )
    .await?;

    // Fire connect in the background so the chip can be captured mid-stream.
    tape.invoke_async(
        "connect_remote_runtime",
        json!({
            "name": config.runtime_name,
            "host": config.host_alias,
            "remoteBinary": config.remote_binary,
            "forwardAgent": false,
        }),
        "connect",
    )
    .await?;

    let installing_selector = format!(
        "[data-testid=remote-server-bundle-installing-{}]",
        config.runtime_name
    );
    let installing_chip = tape
        .wait_for(&installing_selector, Duration::from_secs(10))
        .await?;
    tape.assert("bundle_chip_installing", installing_chip, "");

    tape.scene(
        SceneSpec::new("Auto-install: agent runtime streams to the container, sha-verified, atomic per file")
            .record_sec(4)
            .hold_sec(6),
    )
    .await?;

    let installed_selector = format!(
        "[data-testid=remote-server-bundle-installed-{}]",
        config.runtime_name
    );
    let installed_chip = tape
        .wait_for(&installed_selector, Duration::from_secs(60))
        .await?;
    tape.assert("bundle_chip_installed", installed_chip, "");
    let chip_script = format!(
        r#"var c=document.querySelector({sel}); return c?c.innerText:null;"#,
        sel = serde_json::to_string(&installed_selector)?,
    );
    let chip_text: Option<String> = tape.js(&chip_script).await?;
    tape.log(&format!(
        "chip says: {}",
        chip_text.clone().unwrap_or_default()
    ));

    tape.scene(
        SceneSpec::new(format!(
            "Done — {}. Connect, install, ready in under 10 s.",
            chip_text
                .clone()
                .unwrap_or_else(|| "agent runtime installed".into())
        ))
        .hold_sec(5),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
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
            std::env::remove_var("CONTAINER");
            std::env::remove_var("REMOTE_BINARY");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.host_alias, "helmor-taper-arm64");
        assert_eq!(c.container, "helmor-test-linux-arm64");
        assert_eq!(c.remote_binary, "/home/e2e/.helmor/server/helmor-server");
    }
}
