//! Track B/E/G proof: the Remote Servers row is the operator's per-
//! remote cockpit — Auth (per-runtime SDK key on the daemon),
//! Reconnect (when not connected), Diagnostics (one-click clipboard
//! support bundle), Disconnect. Each affordance is one click; the
//! Auth dialog opens + reports configured/not-configured status; the
//! Diagnostics action fires a toast.
//!
//! Rust port of `scenarios/row-actions.ts`. Non-destructive: the
//! Auth dialog is opened and immediately cancelled so the remote's
//! credentials aren't touched.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ActionButtons {
    pub auth: bool,
    pub reconnect: bool,
    pub diagnostics: bool,
    pub disconnect: bool,
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    let row_selector = format!("[data-testid=remote-server-row-{}]", config.runtime_name);

    tape.js::<Value>(r#"window.location.reload(); return "r";"#)
        .await?;
    tape.sleep(Duration::from_secs(6)).await;
    tape.open_settings("remote-servers").await?;
    let panel_opens = tape
        .wait_for("[role=dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("panel_opens", panel_opens, "");
    let row_present = tape
        .wait_for(&row_selector, Duration::from_secs(10))
        .await?;
    tape.assert("row_present", row_present, "");

    // Confirm the four action buttons are wired.
    let actions_script = format!(
        r#"var q=function(s){{return !!document.querySelector(s)}};
           return {{
             auth: q('[data-testid=remote-server-set-auth-{name}]'),
             reconnect: q('[data-testid=remote-server-reconnect-{name}]'),
             diagnostics: q('[data-testid=remote-server-copy-diagnostics-{name}]'),
             disconnect: q('[data-testid=remote-server-disconnect-{name}]')
           }};"#,
        name = config.runtime_name,
    );
    let actions: ActionButtons = tape.js(&actions_script).await?;
    tape.log(&format!(
        "actions: {}",
        serde_json::to_string(&actions).unwrap_or_default()
    ));
    tape.assert("auth_button", actions.auth, "");
    tape.assert("diagnostics_button", actions.diagnostics, "");
    tape.assert("disconnect_button", actions.disconnect, "");

    tape.scene(
        SceneSpec::new(
            "Settings → Remote Servers: each remote shows its state + one-click Auth · Diagnostics · Disconnect",
        )
        .hold_sec(5),
    )
    .await?;

    // Diagnostics → support-bundle toast.
    tape.click(&format!(
        "[data-testid=remote-server-copy-diagnostics-{}]",
        config.runtime_name
    ))
    .await?;
    let toast = tape
        .wait_for("[data-sonner-toast]", Duration::from_secs(6))
        .await?;
    tape.assert(
        "diagnostics_toast",
        toast,
        if toast { "toast shown" } else { "no toast" },
    );
    tape.scene(
        SceneSpec::new(
            "Diagnostics → one-click clipboard bundle: health snapshot + RPC metrics + last 50 daemon-log lines, JSON formatted",
        )
        .record_sec(2)
        .hold_sec(5),
    )
    .await?;

    // Auth dialog.
    tape.click(&format!(
        "[data-testid=remote-server-set-auth-{}]",
        config.runtime_name
    ))
    .await?;
    let auth_dialog = tape
        .wait_for("[data-testid=runtime-auth-dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("auth_dialog_opens", auth_dialog, "");
    let auth_status: String = tape
        .js(
            r#"var c=document.querySelector('[data-testid=runtime-auth-status-configured]');
               var n=document.querySelector('[data-testid=runtime-auth-status-not-configured]');
               return c?"configured":(n?"not-configured":"unknown");"#,
        )
        .await?;
    tape.assert(
        "auth_status_shown",
        auth_status != "unknown",
        auth_status.clone(),
    );
    tape.scene(
        SceneSpec::new(
            "Auth → per-runtime SDK API key configured ON THE DAEMON. The key never leaves the host; the desktop only sees the configured-providers list.",
        )
        .hold_sec(6),
    )
    .await?;

    // Cancel so we don't write empty creds.
    tape.click("[data-testid=runtime-auth-cancel]").await?;
    tape.sleep(Duration::from_millis(400)).await;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "actions": actions,
        "authStatus": auth_status,
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_default() {
        unsafe { std::env::remove_var("RUNTIME_NAME") };
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
    }

    #[test]
    fn action_buttons_round_trip() {
        let a = ActionButtons {
            auth: true,
            reconnect: false,
            diagnostics: true,
            disconnect: true,
        };
        let wire = serde_json::to_value(&a).unwrap();
        let back: ActionButtons = serde_json::from_value(wire).unwrap();
        assert_eq!(a.auth, back.auth);
        assert_eq!(a.reconnect, back.reconnect);
        assert_eq!(a.diagnostics, back.diagnostics);
        assert_eq!(a.disconnect, back.disconnect);
    }
}
