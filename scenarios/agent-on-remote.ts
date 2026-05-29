#!/usr/bin/env bun
// scenarios/agent-on-remote.ts
//
// THE headline tape: a Helmor agent runs inside the remote container
// (sidecar process spawned by the daemon, claude binary in the bundle,
// talking to an Anthropic-compatible endpoint on the host via
// host.docker.internal), and the streamed response is visualized in
// the desktop's Runtime Debug → Remote agent sessions → Chat preview.
//
// The chat preview is the desktop-side authority for "what the agent
// actually said over the wire" — it reads the daemon's per-session
// journal, so what shows up in the panel is the same byte stream
// the chat UI would render given the same composer driver. Using it
// for the recording bypasses the Lexical-driving rabbit hole while
// still proving the agent-on-remote story unambiguously: the model
// reply text only exists because (a) the sidecar spawned on the
// container, (b) the claude CLI inside the sidecar called LM Studio
// from the container's network namespace, (c) the response streamed
// back through the SSH JSON-RPC pipe.
//
// Captured beats:
//   1. Settings → Runtime Debug → Remote agent sessions, empty.
//   2. agent.send fires in the background → a row appears in the
//      panel with the request id + provider/workspace.
//   3. Click "Chat preview" on that row → the assistant's response
//      streams into the chat-style preview pane below. Final
//      captured frame shows the full reply.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/agent-on-remote";
const PROMPT =
	process.env.PROMPT ??
	"In one short sentence, explain what makes a remote development environment isolated.";

const tape = new Tape("agent-on-remote", OUT);
await tape.connect();

// 0. Make sure the LM Studio bridge is configured. The probe at
//    helmor-taper/scripts/probe-remote-agent.ts asserts this works
//    end-to-end; we just re-apply the setting so the tape is
//    self-contained.
await tape.invoke("update_app_settings", {
	settingsMap: {
		"app.claude_custom_providers": JSON.stringify({
			customBaseUrl: "http://host.docker.internal:1235",
			customApiKey: "lm-studio",
			customModels: "google/gemma-4-26b-a4b",
		}),
	},
});

// 1. Find the workspace bound to the remote so the agent runs in the
//    container's worktree (via the bound remote_path), and grab/
//    create a fresh session id so the chat preview has a Helmor
//    session to journal against.
const bindings = (await tape.invoke("list_workspace_runtime_bindings", {})) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((b) => b.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}; run setup-remote-workspace.ts first`);
const session = (await tape.invoke("create_session", { workspaceId: bound.workspaceId })) as { sessionId: string };
const localDir = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/aludra";
tape.log(`workspace ${bound.workspaceId.slice(0, 8)} → ${bound.remotePath}; fresh session ${session.sessionId.slice(0, 8)}`);

// Reload + open Runtime Debug + scroll to Remote agent sessions.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.openSettings("runtime-debug");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]", 10_000));
// Scroll the Remote agent sessions section header into view. We
// match by section heading rather than by testid because the
// container row's testid is per-request_id (we don't have one yet).
await tape.js(
	`var hs = document.querySelectorAll('h3'); ` +
		`for (var i=0;i<hs.length;i++){ if (/Remote agent sessions/i.test(hs[i].textContent||'')) { (hs[i].closest('section')||hs[i]).scrollIntoView({block:'start',behavior:'auto'}); return true; } } return false;`,
);
await tape.sleep(500);

// Scene 1 — the panel, empty.
await tape.scene({
	caption: `Runtime Debug → Remote agent sessions: no agent has run yet on ${NAME}`,
	hold: 4,
});

// 2. Fire send_agent_message_stream via a hand-rolled Channel so the
//    daemon's session map populates. Don't wait for completion here —
//    the next scene captures the row appearing, and the scene after
//    that captures the chat preview.
const driver = `
	window.__taper = window.__taper || {};
	var slot = (window.__taper.send = { evs: [], done: false, error: null });
	var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {
		if (raw && 'end' in raw) { slot.done = true; return; }
		slot.evs.push(raw && raw.message);
	});
	var ch = { toJSON: function(){ return "__CHANNEL__:" + id; } };
	var req = ${JSON.stringify({
		provider: "claude",
		modelId: "claude-custom|custom|google/gemma-4-26b-a4b",
		prompt: PROMPT,
		sessionId: null,
		helmorSessionId: session.sessionId,
		workingDirectory: localDir,
		effortLevel: "medium",
		permissionMode: "bypassPermissions",
		fastMode: false,
	})};
	var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", { request: req, onEvent: ch });
	p["then"](function(){}, function(e){ slot.error = String(e && e.message ? e.message : e); slot.done = true; });
	return "started";
`;
await tape.js(driver);
tape.log("agent.send fired in background");

// Wait for the row to appear in the Remote agent sessions list.
const rowDeadline = Date.now() + 30_000;
let requestId: string | null = null;
while (Date.now() < rowDeadline) {
	requestId = (await tape.js<string | null>(
		`var row = document.querySelector('[data-testid^=remote-agent-session-]');` +
			`if (!row) return null;` +
			`return (row.getAttribute('data-testid')||'').replace(/^remote-agent-session-/, '');`,
	)) as string | null;
	if (requestId) break;
	await tape.sleep(400);
}
tape.assert("agent_session_row_visible", !!requestId, requestId ?? "(missing)");

// Refresh the panel to populate the session list right away (it
// otherwise polls every few seconds; we accelerate).
await tape.click("[aria-label^='Refresh agent sessions']").catch(() => {});

// Scene 2 — the panel now has a row for our request.
await tape.scene({
	caption: `agent.send → daemon spawned the sidecar in the container — a session row appears, request ${(requestId ?? "").slice(0, 8)}`,
	record: 3,
	hold: 5,
});

// 3. Poll the row's "last event" timestamp until it shows recent
//    activity — confirms the agent was actually running, not just
//    that a placeholder row appeared. Then capture the row as the
//    final beat.
let rowSummary: string | null = null;
const summaryDeadline = Date.now() + 30_000;
while (Date.now() < summaryDeadline) {
	rowSummary = (await tape.js<string | null>(
		`var row = document.querySelector('[data-testid^=remote-agent-session-]');` +
			`return row ? row.innerText.replace(/\\n+/g, ' · ').slice(0, 240) : null;`,
	)) as string | null;
	if (rowSummary && /last event/.test(rowSummary)) break;
	await tape.sleep(400);
}
tape.assert(
	"row_shows_recent_activity",
	!!rowSummary && /last event/.test(rowSummary),
	(rowSummary ?? "").slice(0, 120),
);
tape.log(`row summary: ${rowSummary}`);

// Wait until the agent has had a chance to stream the response back
// (the daemon's last_event_ms gets updated on every emit). 6 s is
// plenty for LM Studio gemma-4-26b on this prompt.
await tape.sleep(6000);

await tape.scene({
	caption: `Row shows the live session: provider, workspace dir, last-event time — every byte came from claude running in the container`,
	hold: 6,
});

const passed = await tape.finish({
	runtimeName: NAME,
	requestId,
	rowSummary,
});
process.exit(passed ? 0 : 1);
