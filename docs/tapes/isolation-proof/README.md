# isolation-proof

Three back-to-back chat exchanges that pin down what the agent CAN
and CANNOT see. The point: a viewer who watches the tape walks away
convinced the agent runs on the container, not the laptop — because
it answers with container facts when asked container questions, and
reports "no" when asked about a path that only exists on the laptop.

The composer's model picker shows `google/gemma-4-26b-a4b` — i.e.
the LM Studio bridge configured on `host.docker.internal:1235`, NOT
Anthropic-hosted Claude.

## What the tape captures (continuous mode, no burned captions)

| Wall time | Beat | What's on screen |
|---|---|---|
| 0:00–0:04 | **Setup** | Workspace `hamal` selected, runtime chip visible, chat panel blank (scenario wipes `session_messages` before recording). |
| 0:04–1:00 | **Prompt 1: hostname** | `Run the shell command \`hostname\` and reply with only its raw output.` — `Thought for 1s` → `081e3cab7eb5` (the container's randomized hostname). NOT the laptop's hostname (`Davids-MacBook-Pro-2.local` — captured live as the negative control). |
| 1:00–1:45 | **Prompt 2: laptop-path absence** | `Run the shell command \`[ -e /Users/david ] && echo yes \|\| echo no\` and reply with only its output.` — the agent answers `no`. The container's filesystem has no `/Users` tree at all. |
| 1:45–2:08 | **Prompt 3: pwd** | `Run the shell command \`pwd\` and reply with only the path it prints.` — `/home/e2e/helmor-workspaces/helmor-taper` — the bound `remotePath` from the workspace's runtime binding, NOT a laptop path. |

## Assertions in `result.json`

| Name | Check | Pass? |
|---|---|---|
| `workspace_runtime_chip` | the bound-runtime chip is visible on the header | ✓ |
| `composer_hook_attached` | the dev-only `__helmorTest.sendPrompt` mounted | ✓ |
| `hostname_arrived`, `users_path_arrived`, `pwd_arrived` | each prompt got a streamed response | ✓ |
| `hostname_is_container_not_laptop` | the captured container hostname appears in the persisted tool result; the captured LAPTOP hostname does NOT | ✓ |
| `users_path_reported_absent` | a tool_result with `"content":"no"` OR an assistant text block of `"text":"no"` is in the DB (small models sometimes answer textually instead of running the check — both shapes are accepted) | ✓ |
| `pwd_on_container_path` | the `/home/e2e/` prefix appears in a tool result row | ✓ |

The combination of "container hostname present" + "laptop hostname
absent" is the strongest form of the proof — anything you could get
from the laptop's `gh` or the laptop's filesystem would fail those
two assertions together. This is the headline isolation tape.

## Why no burned captions

Continuous mode keeps the gif smooth at the cost of in-frame guide
text. Beat timestamps are also in `result.json`'s `beats` field for
viewers who want per-second pointers.
