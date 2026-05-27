#!/usr/bin/env bun
// setup-remote-workspace.ts
//
// Stage a workspace that genuinely RUNS on the docker remote, the way the
// UI's "Move to runtime" does:
//   1. connect the remote (idempotent)
//   2. register the helmor-taper repo (needs a git remote — it's public)
//   3. create a LOCAL workspace + finalize (materializes a local worktree)
//   4. clone_workspace_to_runtime: bundle the LOCAL worktree → clone on the
//      remote → flip the binding to the remote (sets remote_path)
//
// Creating local-then-moving (not create-bound-to-remote) is the supported
// path: the move's bundle source resolves to the LOCAL runtime where the
// worktree actually exists.
//
// Prints { repoId, workspaceId, localDir, remotePath } as JSON.

import { Bridge } from "./mcp-bridge.ts";

const REPO_PATH = Bun.argv[2] ?? new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const BIN = process.env.REMOTE_BINARY ?? "/home/e2e/.helmor/server/helmor-server";
const REMOTE_PATH = process.env.REMOTE_WS_PATH ?? "/home/e2e/helmor-workspaces/helmor-taper";

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 180_000) =>
	b.invokeAndWait(c, a, t, c) as Promise<T>;

// 1. connect (idempotent)
await inv("connect_remote_runtime", { name: NAME, host: HOST, remoteBinary: BIN, forwardAgent: false }).catch((e) => {
	if (!/already registered/.test(String(e))) console.error("connect:", String(e));
});

// 2. repo
const added = (await inv("add_repository_from_local_path", { folderPath: REPO_PATH })) as { repositoryId: string };
const repoId = added.repositoryId;
console.error(`repo: ${repoId}`);

// 3. create LOCAL workspace (no runtimeName) + finalize
const prep = (await inv("prepare_workspace_from_repo", {
	repoId,
	sourceBranch: null,
	mode: null,
	branchIntent: null,
	initialStatus: null,
	runtimeName: null, // <- LOCAL on purpose; we move it next
	seedSessionId: null,
})) as { workspaceId: string };
const workspaceId = prep.workspaceId;
const fin = (await inv("finalize_workspace_from_repo", { workspaceId })) as { workingDirectory: string };
const localDir = fin.workingDirectory;
console.error(`workspace ${workspaceId} finalized locally at ${localDir}`);

// 4. confirm the local worktree materialized before bundling
const { existsSync } = await import("node:fs");
for (let i = 0; i < 40 && !existsSync(localDir); i++) await Bun.sleep(250);
if (!existsSync(localDir)) throw new Error(`local worktree never appeared at ${localDir}`);

// 5. move to the remote (bundle local → clone on container)
const moved = await inv(
	"clone_workspace_to_runtime",
	{ workspaceId, sourceWorkspaceDir: localDir, destinationRuntime: NAME, destinationPath: REMOTE_PATH },
	240_000,
);
console.error(`moved to ${NAME}: ${JSON.stringify(moved).slice(0, 200)}`);

console.log(JSON.stringify({ repoId, workspaceId, localDir, remotePath: REMOTE_PATH, moved }, null, 2));
b.close();
