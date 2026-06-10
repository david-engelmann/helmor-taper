# end-to-end-demo

THE demo. One tape, walks a reviewer through the full user journey
for the remote-runner feature in ~1.8 minutes without reading any
code.

The pre-recording teardown is aggressive: wipes the container's
bundle so the install chip shows real transitions (no "alreadyCurrent"
no-op), plants a `REMOTE_ONLY_MARKER.txt` on the container for the
file-ops beat, clears the workspace's chat history so the panel is
blank when beat 8 switches to the chat surface, pins the workspace
sessions' model column to the LM Studio bridge so the chat beat
doesn't hit Anthropic auth. After the tape finishes, the container
is left in a healthy connected state.

The composer's model picker shows `google/gemma-4-26b-a4b` — the
LM Studio bridge — during the chat beats.

**Post-trimmed:** the raw `master.mov` is ~237 s end-to-end; the
committed `master.mp4` / `master.gif` are ffmpeg-trimmed to ~109 s
by cutting two long LM Studio response windows (roughly 36 s + 47 s
of "Working…" indicator) without losing any visible action — both
"sent" frames and the assistant's text-block reply land in-frame.
Wall time below is the trimmed playback time.

## What the tape captures (continuous mode, no burned captions)

| Wall time | Beat | What's on screen |
|---|---|---|
| 0:00–0:05 | **1. Connected baseline** | Settings → Remote Servers panel, `docker-linux-arm64` row green at "Connected", no agent runtime yet. |
| 0:05–0:13 | **2. Reinstall click → "Uploading"** | Click Reinstall, install chip transitions through `detecting → uploading agent runtime (… MB)`. |
| 0:13–0:23 | **3. Install lands** | Chip flips green: `Agent runtime installed in N.Ns · ready to run agents on the container`. Everything in `$HOME/.helmor/server/`. No sudo. |
| 0:23–0:29 | **5. Workspace bound to remote** | Close the dialog; workspace `hamal` is selected with the blue runtime chip in the header. |
| 0:29–0:42 | **6. File tree from the container** | Open Runtime Debug → Workspace inspector probe → `Run file tree`. The result lists files from `/home/e2e/helmor-workspaces/helmor-taper` on the container, including `REMOTE_ONLY_MARKER.txt` (planted via `docker exec` pre-recording). |
| 0:42–0:57 | **7+8a. Real chat: "list the files"** | The composer goes from empty to a real user prompt. `Thought → text block lists files inline including REMOTE_ONLY_MARKER.txt`. Chat-driven proof of remote execution. *(LM Studio wait trimmed: ~36 s of "Working…" cut between prompt send and response arrival.)* |
| 0:57–1:13 | **8b. Isolation: "hostname?"** | The agent runs `hostname`. Response shows the container's randomized hostname (`081e3cab7eb5`), NOT the laptop's. The chat panel displays both prompts + both replies. *(LM Studio wait trimmed: ~47 s cut.)* |
| 1:13–1:25 | **9. All green** | "All ops route to the container. Your laptop is just the viewport." |
| 1:25–1:38 | **10. docker stop → Degraded banner** | The container is stopped externally; the desktop's liveness ping fails; the top of the window shows `Degraded · docker-linux-arm64 / connection to ssh://helmor-taper-arm64 closed: peer closed connection cleanly (EOF) / Reconnect now`. |
| 1:38–1:49 | **11. docker start + Reconnect → green** | Container started again, Reconnect button clicked, runtime returns to Connected. Same daemon, same workspace, same sessions. |

## Assertions in `result.json`

All 11 assertions passed in the current recording (committed
2026-06-10 — first recording produced by the Rust `taper` binary
after the TS→Rust migration). The hostname beat (`chat_hostname_arrived`
/ `hostname_tool_result_is_container`) lands cleanly in-frame:
container hostname `081e3cab7eb5` shows up in the persisted tool
result; the laptop's hostname is absent. Result: 11/11 ok, 13
beats, scenario passed=true.

For a hostname-focused proof variant, see the `isolation-proof`
tape — three sequential LM Studio turns each pinning what the
agent can and cannot see.

This tape was recorded with the Rust port (`taper scenario
end-to-end-demo`). Earlier copies of this same scenario produced
by the (now-retired) TypeScript implementation are in git history;
result.json shape is byte-for-byte identical.

## Why no burned captions

Continuous mode (`tape.startRecording`) — a single ScreenCaptureKit
pass over the whole scenario. The scenario's `result.json` carries
`beats: [{ t, caption }, ...]` so a viewer who wants per-second
pointers can match the table above to the `t: <seconds>` markers
there.

For single-feature gifs with burned captions, see the older tapes
in this directory (`agent-on-remote`, `remote-file-ops`,
`resilience`, etc.) — those use scene mode with one captioned clip
per beat.
