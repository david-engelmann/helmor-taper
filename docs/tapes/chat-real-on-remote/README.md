# chat-real-on-remote

A user types prompts into the live Helmor composer. Each one resolves
against the agent running on the remote container — the prompts ask
for things that only the container can answer correctly (its files,
its README's content, a file write that lands on its disk).

The composer's model picker shows `google/gemma-4-26b-a4b` —
i.e. the LM Studio bridge configured on `host.docker.internal:1235`,
NOT the laptop's hosted Claude (which would prompt `/login` because
the container has no Anthropic API key).

## What the tape captures (continuous mode, no burned captions)

| Wall time | Beat | What's on screen |
|---|---|---|
| 0:00–0:04 | **Setup** | Workspace `hamal` selected in the sidebar with the `docker-linux-arm64` chip; the chat panel is blank (the scenario wipes `session_messages` for the workspace before recording starts). |
| 0:04–1:20 | **Prompt 1: "List the files…"** | The user message appears, then `Thought for Ns`, then the assistant's text block lists the workspace files inline (Phase 1.1's auto-expand). The listing includes `LICENSE`, `README.md`, `REMOTE_ONLY_MARKER.txt`, `WATCHER_PROOF_*.txt`, `docs`, `scenarios`, `scripts`, `tapes`. The marker file was planted via `docker exec` before recording — its appearance is direct proof the agent ran on the container's filesystem. |
| 1:20–2:00 | **Prompt 2: "Read README.md…"** | The user message appears; the agent thinks (the `Thought for Ns` block expands). Whether a text quote of the README's second line lands depends on the model's run-to-run consistency — the smaller LM Studio bridge sometimes produces only a thinking block on follow-up turns. The DB tool_result for the Read invocation does carry the file content; the visible chat may or may not. |
| 2:00–2:30 | **Prompt 3: "Create HELMOR_DEMO.md…"** | The Write tool invocation. The right-side inspector adds `HELMOR_DEMO.md` to the changes panel. After the recording, `docker exec helmor-test-linux-arm64 cat /home/e2e/helmor-workspaces/helmor-taper/HELMOR_DEMO.md` confirms the file exists on the container with the expected body. |

## On the assertion strictness

`result.json` measures whether each prompt produced a non-empty
assistant text block. Some prompts under the LM Studio bridge end
with a `thinking` block only (no rendered text) even when the
underlying tool ran successfully and persisted its result. So the
strict assertion can fail while the *behavior* is still correct —
the marker file appears in the panel, the new file lands on the
container, and the right-side inspector reflects both. The earlier
chat-real-on-remote run (kept in git history as commit `f019905`)
showed all three prompts producing clean text; this newer run
shows the same for the headline `ls` beat but is less consistent
for the follow-ups. Both are honest snapshots of the LM Studio
bridge's behavior at recording time.

For the cross-cutting "agent ran on the container, not the laptop"
proof, the `isolation-proof` tape is the cleaner artifact.

## Why no burned captions

Continuous mode (`tape.startRecording`) — a single ScreenCaptureKit
pass over the whole scenario. Trade-off vs scene mode: smoother
video, no per-scene cuts, no caption banners. The scenario's
`result.json` carries `beats: [{ t, caption }, ...]` so a viewer
who wants per-second pointers can match this README's table to the
`t: <seconds>` markers there.
