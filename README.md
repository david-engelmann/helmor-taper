# helmor-taper

Evidence-recording toolkit for [Helmor](https://github.com/dohooo/helmor) PRs —
the Helmor analog of [warp-taper](https://github.com/david-engelmann/warp-taper).

It drives the **live Helmor desktop app** through a real scenario, records the
app window with ScreenCaptureKit (window-buffer capture — overlapping apps never
leak into the frame), runs programmatic assertions against the captured IPC +
backend state, and emits a PR-ready bundle (`.mov` / `.mp4` / `.gif` + logs).

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

## Implementation

The crate is pure Rust (no TypeScript scaffolding — the TS implementation was
fully ported across six migration phases R1–R6 and deleted in R6). The only
non-Rust pieces are three small Swift shims that wrap ScreenCaptureKit +
AVFoundation (the macOS-native APIs for window capture + format conversion):

| Path | Purpose |
|---|---|
| `src/bridge/` | WebSocket client for the `tauri-plugin-mcp-bridge` protocol |
| `src/commands.rs` | `execute_js`, `invoke_command`, `poll_result`, `invoke_and_wait`, `capture_screenshot` |
| `src/tape/` | `Tape`, `TapeBuilder`, `SceneSpec`, `ContinuousBeat`, `ResultSummary`, `Recorder` trait + `NullRecorder`, `ScreenCaptureKitRecorder`, post-processing wrappers (`convert_mov_to_mp4`, `convert_mp4_to_gif`) |
| `src/scenarios/` | 13 recorded scenarios (`connect_over_ssh`, `remote_workspace`, `row_actions`, `observability`, `add_remote_wizard`, `resilience`, `first_connect_bundle`, `remote_file_ops`, `remote_runner`, `isolation_proof`, `agent_on_remote`, `chat_real_on_remote`, `end_to_end_demo`) |
| `src/probes/` | 8 headless feature checks (`bundle_install`, `daemon_persistence`, `remote_agent`, `remote_port_forward`, `remote_terminal`, `remote_watch`, `feature_probe`, `setup_remote_workspace`) |
| `src/bin/taper.rs` | top-level CLI |
| `scripts/record-window.swift` | ScreenCaptureKit window-buffer recorder |
| `scripts/mov-to-mp4.swift` | AVFoundation passthrough remux (no re-encode) |
| `scripts/mp4-to-gif.swift` | AVAssetImageGenerator gif encoder (no ffmpeg) |
| `scripts/ssh-config.sh` | bounded `~/.ssh/config` block management |
| `scripts/record-remote-runner.sh` | end-to-end orchestrator (preflight + `taper scenario`) |
| `tapes/` | recorded output bundles (gitignored) |
| `docs/sample-tape/` | one curated, committed example bundle |

## Pipeline

```
preflight  bring up the Dockerized Linux remote (helmor repo's docker-e2e stack)
           + write the ~/.ssh/config block so the desktop can reach it
deploy     launch `bun run dev` (debug build hosts the MCP bridge) and wait
record     ScreenCaptureKit captures the Helmor window for the scenario's life
drive      `taper scenario <name>` runs the scenario steps against the live UI
evaluate   assert RuntimeHealth + IPC events ("remote connected", agent ran)
bundle     mov → mp4 → gif + a PR-ready README referencing the artifacts
```

## Requirements

- macOS 12.3+ (ScreenCaptureKit). Grant **Screen Recording** permission to the
  terminal that runs the recorder (System Settings → Privacy & Security →
  Screen & System Audio Recording).
- Rust 1.94+ (`cargo`).
- Docker (the remote host stack lives in the Helmor repo at
  `src-tauri/tests/docker-e2e/`).
- A Helmor checkout (`HELMOR_SOURCE`, default `~/personal/helmor`).

## Usage

```sh
# Build the taper binary (once).
cargo build --release --bin taper

# Smoke-test the bridge.
./target/release/taper ping

# Run a single scenario against a live Helmor.
./target/release/taper scenario remote-runner
./target/release/taper scenario isolation-proof
./target/release/taper scenario end-to-end-demo

# Run a headless feature check (no recording).
./target/release/taper probe feature-probe
./target/release/taper probe bundle-install
./target/release/taper probe remote-port-forward

# End-to-end orchestration (preflight + scenario + README bundle):
scripts/record-remote-runner.sh

# Overrides:
HELMOR_SOURCE=~/personal/helmor \
TAPE_DIR=tapes/remote-runner \
scripts/record-remote-runner.sh
```

## Available scenarios + probes

```
$ taper                        # prints the full subcommand + scenario + probe list
```

13 scenarios + 8 probes are wired into the CLI. See the help output for the
short descriptions; each scenario / probe's source file has a fuller header
comment.

## Testing

```sh
cargo test          # 116 tests: 91 unit + 10 tape integration + 15 scenario integration
cargo clippy --all-targets -- -D warnings   # clippy-clean
```

The scenario tests use an in-process mock bridge that understands the
fire-and-poll Tauri-command pattern + JS substring matching, so the full
scenario API can be exercised without a live Helmor desktop.

## License

MIT.
