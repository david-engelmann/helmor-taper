# end-to-end-demo

THE demo. One tape, walks a reviewer through the full user journey
for the remote-runner feature in ~2.5 minutes without reading any
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

## What the tape captures (continuous mode, no burned captions)

| Wall time | Beat | What's on screen |
|---|---|---|
| 0:00–0:05 | **1. Connected baseline** | Settings → Remote Servers panel, `docker-linux-arm64` row green at "Connected", no agent runtime yet. |
| 0:05–0:13 | **2. Reinstall click → "Uploading"** | Click Reinstall, install chip transitions through `detecting → uploading agent runtime (… MB)`. |
| 0:13–0:23 | **3. Install lands** | Chip flips green: `Agent runtime installed in N.Ns · ready to run agents on the container`. Everything in `$HOME/.helmor/server/`. No sudo. |
| 0:23–0:29 | **5. Workspace bound to remote** | Close the dialog; workspace `hamal` is selected with the blue runtime chip in the header. |
| 0:29–0:42 | **6. File tree from the container** | Open Runtime Debug → Workspace inspector probe → `Run file tree`. The result lists files from `/home/e2e/helmor-workspaces/helmor-taper` on the container, including `REMOTE_ONLY_MARKER.txt` (planted via `docker exec` pre-recording). |
| 0:42–1:45 | **7+8a. Real chat: "list the files"** | The composer goes from empty to a real user prompt. `Thought → text block lists files inline including REMOTE_ONLY_MARKER.txt`. Chat-driven proof of remote execution. |
| 1:45–2:08 | **8b. Isolation: "hostname?"** | The agent runs `hostname`. Whether the response lands within the recording window is timing-dependent (LM Studio gemma-4-26b-a4b takes 30–60 s per turn under heavy back-to-back load). The prompt firing + `Working...` indicator are visible regardless; the response may complete after the recording cuts. |
| 2:08–2:17 | **9. All green** | "All ops route to the container. Your laptop is just the viewport." |
| 2:17–2:23 | **10. docker stop → Degraded banner** | The container is stopped externally; the desktop's liveness ping fails; the top of the window shows `Degraded · docker-linux-arm64 / ping timed out after 3s / Reconnect now`. |
| 2:23–2:30 | **11. docker start + Reconnect → green** | Container started again, Reconnect button clicked, runtime returns to Connected. Same daemon, same workspace. |

## Assertions in `result.json`

Most pass; the hostname beat (`chat_hostname_arrived` /
`hostname_tool_result_is_container`) may fail when the LM Studio
response runs past the recording's deadline. The headline assertions
covering install + file ops + ls + banner all pass.

For a hostname-focused proof, see the `isolation-proof` tape — that
one's recording window is sized for three sequential LM Studio
turns and consistently lands the hostname response in-frame.

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
