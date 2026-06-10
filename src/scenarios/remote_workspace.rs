//! Visual identification scenario: select the remote-bound workspace
//! and prove the blue runtime chip surfaces in the header (Helmor's
//! per-workspace analog of VS Code's remote indicator).
//!
//! Rust port of `scenarios/remote-workspace.ts`. Assumes a workspace
//! bound to the target runtime already exists — run
//! `setup-remote-workspace.ts` (or its Rust equivalent in Phase R5)
//! beforehand. The scenario discovers the workspace by binding, so
//! the ephemeral workspace ids dev regenerates don't break it.

use std::time::Duration;

use anyhow::{anyhow, Result};
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceBinding {
    pub workspace_id: String,
    pub runtime_name: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ChipState {
    pub present: bool,
    pub label: Option<String>,
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    // Find the workspace bound to the remote.
    let bindings: Vec<WorkspaceBinding> = tape
        .invoke("list_workspace_runtime_bindings", json!({}))
        .await?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| {
            anyhow!(
                "no workspace bound to {}; run setup-remote-workspace first",
                config.runtime_name
            )
        })?;
    let workspace_id = bound.workspace_id.clone();
    tape.log(&format!("bound workspace: {workspace_id}"));

    // Reload to a clean shell, then select the bound workspace.
    tape.js::<Value>(r#"window.location.reload(); return "r";"#)
        .await?;
    tape.sleep(Duration::from_secs(6)).await;
    let row_selector = format!(r#"[data-workspace-row-id="{workspace_id}"]"#);
    let row_present = tape
        .wait_for(&row_selector, Duration::from_secs(10))
        .await?;
    tape.assert("row_present", row_present, "");

    // Click the row body (or the row itself if the body isn't there).
    let click_script = format!(
        r#"var el=document.querySelector({body_sel})||document.querySelector({row_sel}); if(el) el.click(); return "clicked";"#,
        body_sel = serde_json::to_string(&format!(
            r#"[data-workspace-row-id="{workspace_id}"] [data-workspace-row-body]"#
        ))?,
        row_sel = serde_json::to_string(&row_selector)?,
    );
    tape.js::<Value>(&click_script).await?;
    tape.sleep(Duration::from_millis(1500)).await;

    // Confirm the chip is live in the header.
    let chip: ChipState = tape
        .js(
            r#"var c=document.querySelector('[aria-label^="Workspace runtime:"]'); return { present: !!c, label: c?c.getAttribute("aria-label"):null };"#,
        )
        .await?;
    tape.assert(
        "header_chip_visible",
        chip.present,
        chip.label.clone().unwrap_or_else(|| "(none)".into()),
    );
    let chip_names_runtime = chip
        .label
        .as_deref()
        .is_some_and(|l| l.contains(&config.runtime_name));
    tape.assert(
        "chip_names_runtime",
        chip_names_runtime,
        chip.label.clone().unwrap_or_default(),
    );

    // Scene 1 — the bound workspace with the chip in header + sidebar.
    tape.scene(
        SceneSpec::new(format!(
            "This workspace runs on {} — the blue chip marks it in the header & sidebar",
            config.runtime_name
        ))
        .hold_sec(5),
    )
    .await?;

    tape.finish(json!({
        "runtimeName": config.runtime_name,
        "workspaceId": workspace_id,
        "chip": chip,
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_default_is_arm64() {
        unsafe {
            std::env::remove_var("RUNTIME_NAME");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
    }

    #[test]
    fn workspace_binding_deserializes_camel_case_payload() {
        // The backend command returns `workspaceId` / `runtimeName`
        // (camelCase) — Tauri's serde rename. We match that shape.
        let v = json!([{
            "workspaceId": "abc-123",
            "runtimeName": "docker-linux-arm64",
        }]);
        let parsed: Vec<WorkspaceBinding> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].workspace_id, "abc-123");
        assert_eq!(parsed[0].runtime_name, "docker-linux-arm64");
    }

    #[test]
    fn chip_state_round_trips() {
        let c = ChipState {
            present: true,
            label: Some("Workspace runtime: docker-linux-arm64".into()),
        };
        let wire = serde_json::to_string(&c).unwrap();
        let back: ChipState = serde_json::from_str(&wire).unwrap();
        assert_eq!(c.present, back.present);
        assert_eq!(c.label, back.label);
    }

    #[test]
    fn chip_state_absent_form() {
        let v = json!({"present": false, "label": null});
        let c: ChipState = serde_json::from_value(v).unwrap();
        assert!(!c.present);
        assert!(c.label.is_none());
    }
}
