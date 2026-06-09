//! Track B (Setup UX): the "Add remote server" wizard surfaces every
//! SSH affordance an operator expects before committing to a connect
//! — live agent state, identity list, ~/.ssh/config autocomplete +
//! matched-host detail preview, and the agent-forward toggle — and
//! exposes them BEFORE the network call. Mirrors VS Code's "Connect
//! to host" + Zed's remote project flows without re-inventing
//! credential capture (it deliberately reads ~/.ssh/config rather
//! than asking the user to retype it).
//!
//! Rust port of `scenarios/add-remote-wizard.ts`. Non-destructive:
//! the scenario cancels out of the wizard before any network call so
//! it doesn't register a duplicate runtime.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub host_alias: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            host_alias: std::env::var("HOST_ALIAS")
                .unwrap_or_else(|_| "helmor-taper-arm64".into()),
        }
    }
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    tape.js::<Value>(r#"window.location.reload(); return "r";"#).await?;
    tape.sleep(Duration::from_secs(6)).await;

    tape.open_settings("remote-servers").await?;
    let panel_opens = tape
        .wait_for("[role=dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("panel_opens", panel_opens, "");
    tape.click("[data-testid=open-add-remote-server-wizard]")
        .await?;
    let wizard_opens = tape
        .wait_for(
            "[data-testid=add-remote-server-wizard]",
            Duration::from_secs(5),
        )
        .await?;
    tape.assert("wizard_opens", wizard_opens, "");
    let ssh_diagnostics = tape
        .wait_for("[data-testid=ssh-diagnostics]", Duration::from_secs(3))
        .await?;
    tape.assert("ssh_diagnostics_present", ssh_diagnostics, "");
    let id_rows: u64 = tape
        .js(r#"return document.querySelectorAll('[data-testid=ssh-identities-row]').length;"#)
        .await?;
    tape.assert("identities_listed", id_rows > 0, format!("{id_rows} rows"));

    tape.scene(
        SceneSpec::new(
            "Settings → Remote Servers → Add remote server: SSH agent status + identities surface before any network call",
        )
        .hold_sec(5),
    )
    .await?;

    // Type the runtime name + host alias.
    tape.set_input_value("[data-testid=add-remote-server-name]", "demo-arm64")
        .await?;
    tape.sleep(Duration::from_millis(300)).await;
    tape.set_input_value("[data-testid=add-remote-server-host]", &config.host_alias)
        .await?;
    let detail_preview = tape
        .wait_for(
            "[data-testid=add-remote-server-host-detail]",
            Duration::from_secs(4),
        )
        .await?;
    tape.assert("host_detail_preview", detail_preview, "");
    let detail: Option<String> = tape
        .js(
            r#"var d=document.querySelector('[data-testid=add-remote-server-host-detail]');
               return d?d.innerText.replace(/\n+/g," · ").slice(0,180):null;"#,
        )
        .await?;
    tape.log(&format!("host detail: {}", detail.clone().unwrap_or_default()));
    tape.scene(
        SceneSpec::new(format!(
            "Typing \"{}\" matches an ~/.ssh/config entry — Helmor previews hostname/port/identity straight from your config",
            config.host_alias
        ))
        .hold_sec(6),
    )
    .await?;

    // Toggle agent-forward.
    tape.click("[data-testid=add-remote-server-forward-agent-input]")
        .await?;
    let forward_checked: bool = tape
        .js(r#"return !!document.querySelector('[data-testid=add-remote-server-forward-agent-input]')?.checked;"#)
        .await?;
    tape.assert("forward_agent_toggled", forward_checked, "");
    tape.scene(
        SceneSpec::new(
            "Forward SSH agent → the daemon will inherit your local keys for git fetch/push on private repos",
        )
        .hold_sec(5),
    )
    .await?;

    // Cancel to keep the scenario non-destructive.
    tape.click("[data-testid=add-remote-server-cancel]").await?;
    tape.sleep(Duration::from_millis(400)).await;

    tape.finish(json!({
        "hostAlias": config.host_alias,
        "identityRows": id_rows,
        "hostDetail": detail,
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_default_is_arm64_alias() {
        unsafe { std::env::remove_var("HOST_ALIAS") };
        assert_eq!(Config::from_env().host_alias, "helmor-taper-arm64");
    }
}
