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
