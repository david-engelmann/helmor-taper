//! Proves the core promise: when a workspace is bound to a remote
//! runtime, every file-op (file tree, changes, file read, status)
//! automatically runs on the CONTAINER, not the laptop. Uses Runtime
//! Debug → Workspace inspector probe to round-trip
//! `workspace.fileTree` + `workspace.changes` through the resolved
//! runtime, with Auto-via-binding flipping the call onto the bound
//! remote by virtue of the workspace's `remote_path`.
//!
//! Rust port of `scenarios/remote-file-ops.ts`. Plants a
//! `REMOTE_ONLY_MARKER.txt` on the container so the second probe
//! conclusively shows the call hit the container (the marker doesn't
//! exist in the local worktree).

use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tape::{SceneSpec, Tape};

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_name: String,
    pub container: String,
    pub local_workspace_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            container: std::env::var("CONTAINER")
                .unwrap_or_else(|_| "helmor-test-linux-arm64".into()),
            local_workspace_dir: std::env::var("LOCAL_WS_DIR").unwrap_or_else(|_| {
                "/Users/david/helmor-dev/workspaces/helmor-taper/alnitak".into()
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceBinding {
    pub workspace_id: String,
    pub runtime_name: String,
    pub remote_path: String,
}

pub async fn run(tape: &mut Tape, config: &Config) -> Result<bool> {
    let bindings: Vec<WorkspaceBinding> = tape
        .invoke("list_workspace_runtime_bindings", json!({}))
        .await?;
    let bound = bindings
        .iter()
        .find(|b| b.runtime_name == config.runtime_name)
        .ok_or_else(|| anyhow!("no workspace bound to {}", config.runtime_name))?;
    tape.log(&format!(
        "bound: {}… → {}",
        &bound.workspace_id[..bound.workspace_id.len().min(8)],
        bound.remote_path
    ));

    tape.js::<Value>(r#"window.location.reload(); return "r";"#).await?;
    tape.sleep(Duration::from_secs(6)).await;
    tape.open_settings("runtime-debug").await?;
    let panel_opens = tape
        .wait_for("[role=dialog]", Duration::from_secs(5))
        .await?;
    tape.assert("panel_opens", panel_opens, "");
    tape.sleep(Duration::from_millis(400)).await;
    let scrolled = tape.scroll_to_section("#inspector-probe-workspace").await?;
    tape.assert("probe_section_scrolled", scrolled, "");
    tape.sleep(Duration::from_millis(400)).await;

    // Fill the form.
    tape.set_input_value("#inspector-probe-workspace-id", &bound.workspace_id)
        .await?;
    tape.set_input_value("#inspector-probe-workspace", &config.local_workspace_dir)
        .await?;
    tape.sleep(Duration::from_millis(300)).await;

    tape.scene(
        SceneSpec::new(format!(
            "Workspace inspector probe → workspace ID + local worktree path. Runtime = \"Auto (via binding)\" → calls route via remote_path on {}",
            config.runtime_name
        ))
        .hold_sec(5),
    )
    .await?;

    // Run file tree.
    let tree_clicked = tape.click_button_by_text("Run file tree").await?;
    tape.assert("file_tree_clicked", tree_clicked, "");
    let tree_rendered = tape
        .wait_for_text(
            "[role=dialog]",
            "files (showing first",
            Duration::from_secs(15),
        )
        .await?;
    tape.assert("file_tree_rendered", tree_rendered, "");
    let tree_preview: Option<String> = tape
        .js(
            r#"var lis=document.querySelectorAll('[role=dialog] li');
               var paths=[]; for(var i=0;i<lis.length && i<6;i++){paths.push(lis[i].innerText.trim());}
               return paths.join(' · ');"#,
        )
        .await?;
    tape.log(&format!(
        "file tree preview: {}",
        tree_preview.clone().unwrap_or_default()
    ));
    tape.scene(
        SceneSpec::new(format!(
            "Run file tree → entries returned from the container worktree at {} — same call shape, remote answer",
            bound.remote_path
        ))
        .hold_sec(6),
    )
    .await?;

    // Plant a remote-only marker.
    const MARKER: &str = "REMOTE_ONLY_MARKER.txt";
    let marker_text = format!(
        "remote-proof-{}",
        std::process::id() // ID-stable; avoids the time-fn ban in scenario code
    );
    let plant_cmd = format!(
        "printf '%s' '{marker_text}' > '{}/{MARKER}'",
        bound.remote_path,
    );
    let plant = Command::new("docker")
        .args(["exec", &config.container, "sh", "-c", &plant_cmd])
        .output()?;
    if !plant.status.success() {
        return Err(anyhow!(
            "plant marker: docker exec exit {}: {}",
            plant.status,
            String::from_utf8_lossy(&plant.stderr).trim()
        ));
    }
    tape.log(&format!("planted {MARKER} on container"));

    let changes_clicked = tape.click_button_by_text("Run changes").await?;
    tape.assert("changes_clicked", changes_clicked, "");
    let changes_rendered = tape
        .wait_for_text("[role=dialog]", "changed path", Duration::from_secs(15))
        .await?;
    tape.assert("changes_rendered", changes_rendered, "");
    let saw_marker_script = format!(
        r#"return ((document.querySelector('[role=dialog]')||{{}}).innerText||'').indexOf({m})>=0;"#,
        m = serde_json::to_string(MARKER)?,
    );
    let saw_marker: bool = tape.js(&saw_marker_script).await?;
    tape.assert(
        "marker_in_changes",
        saw_marker,
        if saw_marker {
            "marker visible in changes list"
        } else {
            "marker missing"
        },
    );
    tape.scene(
        SceneSpec::new(format!(
            "Planted {MARKER} on the container → Run changes lists it as untracked. Proof: the call hit the container, not the local worktree."
        ))
        .hold_sec(6),
    )
    .await?;

    tape.finish(json!({
        "workspaceId": bound.workspace_id,
        "remotePath": bound.remote_path,
        "marker": MARKER,
        "treePreview": tree_preview,
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
            std::env::remove_var("LOCAL_WS_DIR");
        }
        let c = Config::from_env();
        assert_eq!(c.runtime_name, "docker-linux-arm64");
        assert_eq!(c.container, "helmor-test-linux-arm64");
        assert!(c.local_workspace_dir.starts_with("/Users/david"));
    }

    #[test]
    fn workspace_binding_deserializes_with_remote_path() {
        let v = json!([{
            "workspaceId": "abc-123",
            "runtimeName": "docker-linux-arm64",
            "remotePath": "/home/e2e/helmor-workspaces/helmor-taper",
        }]);
        let parsed: Vec<WorkspaceBinding> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed[0].remote_path, "/home/e2e/helmor-workspaces/helmor-taper");
    }
}
