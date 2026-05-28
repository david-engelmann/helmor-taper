# Remote-runner feature evidence catalog

The reproducible map of **every intentional remote-runner feature** → how to
**drive** it (the navigation to record) + how to **prove** it (the assertion
that confirms it works). Two ways to use it:

- **Confirm now (headless):** `bun scripts/feature-probe.ts` invokes the same
  backend commands the UI uses against a live Helmor connected to the
  Dockerized Linux remote, and asserts on the results. A green run proves the
  feature works end-to-end (desktop → SSH → daemon → container) without
  recording. Last run: **19/19 command-level checks pass** + resilience
  confirmed separately.
- **Record later:** the "Drive (UI)" column is the on-screen navigation to
  capture with the scene recorder (`scenarios/lib.ts` + `capture-scene.ts`),
  one captioned scene per feature.

## Preconditions

1. `bun run dev` in the Helmor checkout (debug build → MCP bridge on :9223).
2. The Dockerized Linux remote up + a workspace moved onto it:
   `bun scripts/setup-remote-workspace.ts` (brings up the container, registers
   the public `helmor-taper` repo, creates a local workspace, and moves it onto
   the remote via bundle/clone).
3. The remote daemon reachable via the `helmor-taper-arm64` ssh-config alias
   (`scripts/ssh-config.sh add helmor-taper-arm64 2223 <id_e2e>`).

## How "proof" is made rigorous

The local and remote worktrees are the *same repo*, so reading `README.md`
from either looks identical. To prove an op truly hits the **container**, the
probe plants a `REMOTE_ONLY_MARKER.txt` (unique content) at the remote worktree
via `docker exec`, then asserts the op sees it. Runtime health is asserted to
report `kind=remote` + the container hostname (`1a51913e7039`), not the laptop.

---

## Track B — Setup UX (Zed / VS Code parity)

| Feature | Claim | Drive (UI) | Prove (command + assertion) | Status |
|---|---|---|---|---|
| **B1** Add-Remote-Server wizard | Add a remote in <2 min: host field, live SSH diagnostics, agent-forward toggle, pre-flight probe, Connect | Settings → Remote Servers → "Add remote server" → fill host → Connect | `connect_remote_runtime{name,host,remoteBinary}` → returns `RuntimeHealth` | ✅ confirmed |
| **B2** `~/.ssh/config` integration | Host autocomplete + hostname/port/user/identity preview from the user's ssh config | Wizard host field shows config hosts + detail preview | `list_ssh_hosts` ⊇ alias; `list_ssh_host_details` has hostname/port | ✅ confirmed |
| **B3** SSH key + agent diagnostics + pre-connect probe | Surface identities + agent socket state; classify reachable / auth-fail / timeout before connecting | Wizard: identity list, SSH-agent chip, probe result | `list_ssh_identities` (n≥1); `ssh_agent_status` (`state:available`, keys_loaded); `probe_ssh_host{host}` (`state:reachable`, latency) | ✅ confirmed |
| **B5** Sidebar / header host indicator | A bound workspace is unmistakably marked as remote (blue runtime chip), everywhere | Select a remote-bound workspace → chip in header + sidebar row + terminal corner | `list_workspace_runtime_bindings` shows the binding; chip = `[aria-label^="Workspace runtime:"]` | ✅ confirmed (+ enhanced this pass) |
| Empty-state CTA | Remote Servers panel guides first connect | Settings → Remote Servers (no remotes) → "Add a remote server" CTA | `[data-testid=remote-servers-empty]` present | ✅ confirmed (connect tape) |

## Track C — Resilience

| Feature | Claim | Drive (UI) | Prove | Status |
|---|---|---|---|---|
| **C** Reconnect after drop | A dropped SSH connection re-establishes; chat banner offers Reconnect | Stop the remote → banner appears → network heals / Reconnect → green | stop container → `reconnect_remote_runtime{name}` → `RuntimeHealth`; `list_remote_runtimes` state → `connected` | ✅ confirmed (manual reconnect; auto-loop timing-based) |

## Track D — Distribution

| Feature | Claim | Drive (UI) | Prove | Status |
|---|---|---|---|---|
| **D3/D4** Auto-install + protocol negotiation | First connect installs `helmor-server`; protocol mismatch triggers reinstall | Connect to a host without the daemon → "installing…" → connected | `connect_remote_runtime` logs `helmor-server present at requested path … protocol 0.1.0`; `reinstall_remote_daemon{name}` available | ◑ install-detect confirmed; full fresh-install path needs a daemon-less host |

## Track E — Observability

| Feature | Claim | Drive (UI) | Prove | Status |
|---|---|---|---|---|
| **E1** Daemon log tail | Tail the remote daemon's log from the Runtime Debug panel | Settings → Runtime Debug → Log tail | `tail_remote_daemon_log{name,maxLines}` → `{lines:[]}` | ✅ confirmed |
| **E2** Per-method RPC metrics | p50/p99 + counters per RPC method | Runtime Debug → Metrics table | `get_remote_runtime_metrics{name}` → `{methods, uptimeSecs, recentStartsMs}` | ✅ confirmed |
| **E3** Copy-diagnostics bundle | One-click JSON blob: health + metrics + last log lines for support | Remote Servers row → "Diagnostics" (Copy) | `get_remote_runtime_diagnostics{name}` → `{name,state,health,client,lastPingMs,agentSessionCount}` | ✅ confirmed |

## Track F — Multi-host

| Feature | Claim | Drive (UI) | Prove | Status |
|---|---|---|---|---|
| **F2** Per-host worktree path | Each (workspace,runtime) remembers its remote worktree path | Move workspace → path stored on the binding | `list_workspace_runtime_bindings` → `remotePath` set | ✅ confirmed |
| **F2.1** Path memory across rebinds | The remembered path survives rebinding elsewhere + pre-fills on reopen | Move-to-runtime dialog pre-fills the prior remote path | `get_remembered_workspace_remote_path{workspaceId,runtimeName}` == remotePath | ✅ confirmed |
| **F3** Cross-host workspace move | Bundle the worktree + clone onto the destination runtime | Sidebar → "Move to runtime" → pick remote → progress → bound | `clone_workspace_to_runtime{workspaceId,sourceWorkspaceDir,destinationRuntime,destinationPath}` → `{cloned:true, headBranch, remotePath}`; worktree appears on container | ✅ confirmed |

## Track G — Auth & secrets

| Feature | Claim | Drive (UI) | Prove | Status |
|---|---|---|---|---|
| **G2** Per-runtime agent auth status | Show which providers have a key configured on the daemon (key never leaves it) | Remote Servers row → "Auth" | `get_remote_runtime_auth_status{name}` → `{providers:[…]}` | ✅ confirmed (empty until `set_runtime_agent_auth`) |

## Core — bound workspace operates over the wire

All file-ops translate the local worktree path → the binding's `remote_path`
(via `ResolvedRuntime::translate_workspace_dir`) and run on the container.

| Feature | Prove (command, run on the remote worktree) | Status |
|---|---|---|
| Remote git **status** | `get_workspace_status` → sees the remote-only marker (untracked on the container) | ✅ confirmed |
| Remote **branch info** | `get_workspace_branch_info` → `currentBranch:main`, `headCommit` = the remote clone's HEAD | ✅ confirmed |
| Remote **file read** | `read_workspace_file{relativePath:REMOTE_ONLY_MARKER.txt}` → content matches the planted marker | ✅ confirmed |
| Remote **file tree** | `get_workspace_file_tree` → 19 entries incl. the remote-only marker | ✅ confirmed |
| Remote **search** (git grep) | `search_workspace{query:"Helmor"}` → matches from the container's README.md | ✅ confirmed |
| Remote **read-at-ref** (diff base) | `read_workspace_file_at_ref{gitRef:HEAD}` → README from the remote ref | ✅ confirmed |
| Remote **runtime health** | `get_runtime_health{runtimeName}` → `kind=remote`, hostname = container id | ✅ confirmed |

## Agent execution on the remote (the headline)

| Feature | Status |
|---|---|
| `agent.send` routes to the daemon + spawns the sidecar **in the correct remote worktree** | ✅ confirmed (after the working-dir fix; daemon logs "agent bridge configured") |
| Agent CLI actually runs on the remote (Claude Code via LM Studio's Anthropic endpoint) | ⏳ blocked: needs a **Linux-native** `helmor-sidecar` (cross-compiling on macOS embeds macOS native modules → `node-pty` crash). Build it on Linux to finish. |

See `../README.md` for the recorder/driver, and `docs/tapes/` for the captioned
gifs (connect-over-ssh, remote-workspace). `scripts/feature-probe.ts` is the
runnable confirmation behind this catalog.

## Bugs found + fixed while building this (in the helmor repo)

The systematic confirmation surfaced real path-resolution bugs, since fixed:
- `start_workspace_watch` used the local path on the remote → watch failed every
  workspace-open. Now translates via `remote_path`.
- `send_agent_message_stream` shipped the local cwd to `agent.send` → agent had
  nowhere valid to run. Now translates via `resolve_remote_workspace_dir`.
- (Plus 5 merge-regression fixes: `update_app_settings`/teardown `Arc` state,
  test-setup typecheck, Docker `cmake`/`clang`.)
