# helmor-taper

Evidence-recording toolkit for [Helmor](https://github.com/dohooo/helmor) PRs —
the Helmor analog of [warp-taper](https://github.com/david-engelmann/warp-taper).

It drives the **live Helmor desktop app** through a real scenario, records the
app window with ScreenCaptureKit (window-buffer capture — overlapping apps never
leak into the frame), runs programmatic assertions against the captured IPC +
backend state, and emits a PR-ready bundle (`.mov` / `.mp4` / `.gif` + logs).

## Migration in progress: TypeScript → Rust

The current implementation is TypeScript (`scenarios/*.ts`, `scripts/*.ts`) +
Swift for ScreenCaptureKit (`scripts/record-window.swift`,
`scripts/{mov-to-mp4,mp4-to-gif}.swift`) + Bash for orchestration. A Rust
rewrite landed alongside it in Phase R1 (the MCP bridge client crate). The
TypeScript scaffolding stays in place until the Rust port reaches feature
parity; then it gets removed in one sweep.

Today (Phase R4 — first two scenarios ported, infra for the rest):
- `cargo build --all-targets` → clean.
- `cargo run --bin taper -- scenario connect-over-ssh` → drives the
  full SSH-connect flow against a live Helmor desktop (when
  `bun run dev` is up). `TAPE_DIR=./tapes/foo` overrides the output.
- `cargo test` → **72 / 72 passing**:
  - R1+R2+R3 (57): same coverage as the prior README.
  - R4 (15): 4 scenario unit tests (`Config::from_env` defaults,
    semver-shape predicate, `DaemonHealth` partial deserialization)
    × 2 scenarios = 8 unit tests, plus 1 added Tape unit test for
    "scene without start_recording is a no-op", plus 4 scenario
    integration tests against a smarter mock bridge that understands
    the fire-and-poll pattern + JS-substring matching, plus 2 hooked
    bridge connect-result tests.
- Two scenarios ported:
  - `scenarios/connect-over-ssh.ts` → `src/scenarios/connect_over_ssh.rs`
    (143 → ~250 LOC including config struct + semver check + 4 tests).
  - `scenarios/remote-workspace.ts` → `src/scenarios/remote_workspace.rs`
    (54 → ~150 LOC including config + 4 tests).

The scenario test harness in `tests/scenarios_integration.rs` is the
reusable piece for the remaining 12 scenarios: a programmable
`MockState` with two extensibility points (Tauri command responses +
JS substring matchers). New scenarios add tests by registering their
expected commands + selectors in `MockState` rather than building
bridge plumbing from scratch.

Older state (kept for reference until Phase R6 deletes the TS scaffolding):
  - R1 (15): 7 protocol unit tests + 8 client unit tests (port-scan, echo round-trip, concurrent fan-out, timeout, error-frame surfacing).
  - R2 (28): 6 commands unit tests + 14 tape unit tests + 8 tape integration tests against a mock bridge.
  - R3 (14): 7 ScreenCaptureKit recorder tests (happy path, non-zero exit surfaces stderr, double-start errors, wait-before-start errors, missing binary, Drop reaps orphan child, arg layout sanity) + 4 post-processing unit tests (mov→mp4 happy path, mp4→gif fps/maxWidth pass-through, non-zero exit propagates stderr + tool name, missing binary surfaces tool name) + 2 new end-to-end integration tests (full continuous-mode pipeline with record + post-processing wired via shell shims; post-processing failure propagates through Tape::finish).
- `cargo run --bin taper -- ping` → smoke-tests against a live Helmor's MCP bridge if `bun run dev` is up.

What's in:
- `src/bridge/{mod,protocol,client}.rs` — WebSocket client (Phase R1).
- `src/commands.rs` — `execute_js`, `invoke_command`, `poll_result`, `invoke_and_wait`, `capture_screenshot` (Phase R2).
- `src/tape/{mod,assertion,recorder}.rs` — `Tape`, `TapeBuilder`, `SceneSpec`, `ContinuousBeat`, `ResultSummary`, `Recorder` trait + `NullRecorder` (Phase R2).
- `src/tape/screencapturekit.rs` — `ScreenCaptureKitRecorder` (Phase R3); spawns `scripts/record-window.swift` with the same arg layout the TS port uses, captures stderr for diagnostic surfacing, reaps the child on Drop so a scenario panic doesn't leak the swift process.
- `src/tape/post.rs` — `convert_mov_to_mp4` + `convert_mp4_to_gif` wrappers around the two Swift conversion shims, with structured errors that name the failing tool + capture stderr (Phase R3).
- `PostProcessing` struct on `TapeBuilder` — optional config that wires the mov→mp4→gif chain into `Tape::finish`. Continuous-mode tapes now produce all three artifacts end-to-end (Phase R3).
- `tests/tape_integration.rs` — end-to-end Tape API exercised against an in-process mock bridge (Phase R2 + R3).

What's stubbed (Phase R4 fills in):
- Scene mode (per-clip capture with burned-in captions) errors with "not yet implemented" — the headline tapes use continuous mode and don't need this; the older per-feature tapes do.
- No scenarios have been ported to Rust yet — the TypeScript `scenarios/*.ts` still drive the actual recording sessions until Phase R4 ports them one by one.

The TypeScript implementation (`scripts/mcp-bridge.ts`, `scenarios/*.ts`)
stays in place during the migration. README note will get pruned once
the Rust port reaches parity in Phase R6.

The flagship scenario is the **remote-runner**: a Dockerized Linux host running
`helmor-server` over SSH, with the desktop connecting to it, going green, and
running an agent on the remote — the whole point of the remote-runner feature,
proven on video against a real remote.

## Why a separate repo

Same reason warp-taper is separate from warp: the recorder, its Swift helpers,
and the (large, regenerated) tape artifacts have no business in the Helmor PR.
helmor-taper points *at* a Helmor checkout; it never ships inside it.

## How it differs from warp-taper

Warp is an opaque terminal, so warp-taper drives it with OCR-gated `CGEventPost`
(synthetic clicks/keystrokes anchored on Vision OCR). Helmor is a Tauri webview
whose debug build hosts the **`tauri-plugin-mcp-bridge`** WebSocket on
`127.0.0.1:9223`. helmor-taper drives through that bridge instead:

| Capability | Mechanism |
|---|---|
| Find / record the window | `list_windows` + `scripts/record-window.swift` (ScreenCaptureKit) |
| Navigate / click the UI | `execute_js` against real `data-testid` elements |
| Invoke backend commands | `execute_js` → `window.__TAURI_INTERNALS__.invoke(...)` |
| Assert on behavior | `invoke_tauri` IPC monitor + backend-state reads |

DOM-anchored driving is deterministic in a way OCR isn't — we click the actual
`remote-server-reconnect-<name>` button, not "whatever pixels look like it."

## Pipeline

```
preflight  bring up the Dockerized Linux remote (helmor repo's docker-e2e stack)
           + write the ~/.ssh/config block so the desktop can reach it
deploy     launch `bun run dev` (debug build hosts the MCP bridge) and wait
record     ScreenCaptureKit captures the Helmor window for the scenario's life
drive      mcp-bridge driver runs the scenario steps against the live UI
evaluate   assert RuntimeHealth + IPC events ("remote connected", agent ran)
bundle     mov → mp4 → gif + a PR-ready README referencing the artifacts
```

## Layout

| Path | Purpose |
|---|---|
| `scripts/mcp-bridge.ts` | Bun client for the MCP-bridge WebSocket protocol |
| `scripts/record-window.swift` | ScreenCaptureKit window-buffer recorder (from warp-taper) |
| `scripts/mov-to-mp4.swift` / `mp4-to-gif.swift` | format converters (from warp-taper) |
| `scripts/ssh-config.sh` | bounded `~/.ssh/config` block management for the docker host |
| `scripts/record-remote-runner.sh` | the end-to-end orchestrator |
| `scenarios/` | scenario drivers (Bun) — `remote-runner.ts` is the flagship |
| `tapes/` | recorded output bundles (gitignored) |
| `docs/sample-tape/` | one curated, committed example bundle |

## Requirements

- macOS 12.3+ (ScreenCaptureKit). Grant **Screen Recording** permission to the
  terminal that runs the recorder (System Settings → Privacy & Security →
  Screen & System Audio Recording).
- [Bun](https://bun.sh) 1.3+ (driver + scenarios).
- Docker (the remote host stack lives in the Helmor repo at
  `src-tauri/tests/docker-e2e/`).
- A Helmor checkout (`--helmor-source`, default `~/personal/helmor`).

## Usage

```sh
# Bring up the remote, launch + drive Helmor, record the tape:
scripts/record-remote-runner.sh

# Overrides:
HELMOR_SOURCE=~/personal/helmor \
DURATION_S=45 \
TAPE_DIR=tapes/remote-runner \
scripts/record-remote-runner.sh
```

## License

MIT.
