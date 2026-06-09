//! One-shot environment setup: stage a workspace that genuinely
//! RUNS on the remote, the way the UI's "Move to runtime" does:
//! 1. connect the remote (idempotent)
//! 2. register the helmor-taper repo
//! 3. create a LOCAL workspace + finalize (materializes a local worktree)
//! 4. clone_workspace_to_runtime: bundle local → clone on the remote
//!    → flip the binding to the remote (sets remote_path)
//!
//! Rust port of `scripts/setup-remote-workspace.ts`. Lives under
//! `probes` rather than `scenarios` because it doesn't record video
//! — it's the env-prep step you run once before recording any tape.
//! Prints a JSON summary to stdout (so it can be piped into other
//! tooling) + progress to stderr.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::bridge::Bridge;
use crate::commands::invoke_and_wait;

#[derive(Debug, Clone)]
pub struct Config {
    /// Local path to the helmor-taper repo checkout. Defaults to the
    /// crate root (one level up from `scripts/`).
    pub repo_path: PathBuf,
    pub host_alias: String,
    pub runtime_name: String,
    pub remote_binary: String,
    pub remote_workspace_path: String,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            repo_path: std::env::var("REPO_PATH")
                .ok()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                }),
            host_alias: std::env::var("HOST_ALIAS").unwrap_or_else(|_| "helmor-taper-arm64".into()),
            runtime_name: std::env::var("RUNTIME_NAME")
                .unwrap_or_else(|_| "docker-linux-arm64".into()),
            remote_binary: std::env::var("REMOTE_BINARY")
                .unwrap_or_else(|_| "/home/e2e/.helmor/server/helmor-server".into()),
            remote_workspace_path: std::env::var("REMOTE_WS_PATH")
                .unwrap_or_else(|_| "/home/e2e/helmor-workspaces/helmor-taper".into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddRepoResult {
    repository_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareWorkspace {
    workspace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinalizeWorkspace {
    working_directory: String,
}

pub async fn run(bridge: &Bridge, config: &Config) -> Result<bool> {
    let timeout = Duration::from_secs(180);

    // 1. connect (idempotent — swallow "already registered").
    if let Err(e) = invoke_and_wait(
        bridge,
        "connect_remote_runtime",
        json!({
            "name": config.runtime_name,
            "host": config.host_alias,
            "remoteBinary": config.remote_binary,
            "forwardAgent": false,
        }),
        timeout,
        "setup-conn",
    )
    .await
    {
        let msg = e.to_string();
        if !msg.contains("already registered") {
            eprintln!("connect: {msg}");
        }
    }

    // 2. repo.
    let added: AddRepoResult = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "add_repository_from_local_path",
            json!({"folderPath": config.repo_path}),
            timeout,
            "setup-add-repo",
        )
        .await?,
    )?;
    let repo_id = added.repository_id;
    eprintln!("repo: {repo_id}");

    // 3. create LOCAL workspace (no runtimeName) + finalize.
    let prep: PrepareWorkspace = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "prepare_workspace_from_repo",
            json!({
                "repoId": repo_id,
                "sourceBranch": Value::Null,
                "mode": Value::Null,
                "branchIntent": Value::Null,
                "initialStatus": Value::Null,
                "runtimeName": Value::Null,
                "seedSessionId": Value::Null,
            }),
            timeout,
            "setup-prepare",
        )
        .await?,
    )?;
    let workspace_id = prep.workspace_id;

    let fin: FinalizeWorkspace = serde_json::from_value(
        invoke_and_wait(
            bridge,
            "finalize_workspace_from_repo",
            json!({"workspaceId": workspace_id}),
            timeout,
            "setup-finalize",
        )
        .await?,
    )?;
    let local_dir = fin.working_directory;
    eprintln!("workspace {workspace_id} finalized locally at {local_dir}");

    // 4. confirm the local worktree materialized.
    let local_path = PathBuf::from(&local_dir);
    let mut waited = 0;
    while !local_path.exists() && waited < 40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        waited += 1;
    }
    if !local_path.exists() {
        return Err(anyhow!("local worktree never appeared at {local_dir}"));
    }

    // 5. clone to the remote (bundle local → clone on container).
    let moved = invoke_and_wait(
        bridge,
        "clone_workspace_to_runtime",
        json!({
            "workspaceId": workspace_id,
            "sourceWorkspaceDir": local_dir,
            "destinationRuntime": config.runtime_name,
            "destinationPath": config.remote_workspace_path,
        }),
        Duration::from_secs(240),
        "setup-clone",
    )
    .await?;
    eprintln!(
        "moved to {}: {}",
        config.runtime_name,
        serde_json::to_string(&moved)?
            .chars()
            .take(200)
            .collect::<String>()
    );

    // Emit the JSON summary on stdout — match the TS port's contract.
    let summary = json!({
        "repoId": repo_id,
        "workspaceId": workspace_id,
        "localDir": local_dir,
        "remotePath": config.remote_workspace_path,
        "moved": moved,
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_defaults() {
        unsafe {
            std::env::remove_var("REPO_PATH");
            std::env::remove_var("HOST_ALIAS");
            std::env::remove_var("RUNTIME_NAME");
            std::env::remove_var("REMOTE_BINARY");
            std::env::remove_var("REMOTE_WS_PATH");
        }
        let c = Config::from_env();
        assert!(c.repo_path.is_absolute());
        assert_eq!(c.host_alias, "helmor-taper-arm64");
        assert_eq!(c.remote_workspace_path, "/home/e2e/helmor-workspaces/helmor-taper");
    }

    #[test]
    fn add_repo_result_deserializes_camel_case() {
        let v = json!({"repositoryId": "repo-abc"});
        let parsed: AddRepoResult = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.repository_id, "repo-abc");
    }
}
