#!/usr/bin/env bun
// probe-daemon-persistence.ts
//
// Confirms the persistent-daemon promise that the shell-quoting fix
// unlocked: a remote agent session started on the daemon is still
// visible AFTER a forced SSH disconnect + reconnect. The daemon survives
// the per-session proxy churn because it's a double-forked child of init
// (not of the SSH `helmor-server.real` parent). Without persistence the
// new proxy would talk to a fresh daemon with no memory of prior runs.
//
// Sequence:
//   1. fire `agent.send` on a remote-bound workspace, wait for the
//      stream to complete (or land in the journal as `endedReplayOnly`)
//   2. `list_remote_agent_sessions` → capture the request id
//   3. snapshot the daemon's PID on the container (via `docker exec`)
//   4. `disconnect_remote_runtime` (kills the SSH session)
//   5. `connect_remote_runtime` (new SSH session → new proxy → SAME daemon)
//   6. snapshot the daemon's PID again — MUST match step 3
//   7. `list_remote_agent_sessions` again → the session id MUST still be there

import { Bridge } from "./mcp-bridge.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const BIN = process.env.REMOTE_BINARY ?? "/home/e2e/.helmor/server/helmor-server";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 60_000) =>
	b.invokeAndWait(c, a, t, `pdp-${c}`) as Promise<T>;

async function daemonPid(): Promise<string | null> {
	const p = Bun.spawn([
		"docker", "exec", CONTAINER, "sh", "-c",
		`pgrep -f 'helmor-server.real --daemon' | head -1`,
	], { stdout: "pipe" });
	const out = (await new Response(p.stdout).text()).trim();
	return out.length > 0 ? out : null;
}

// 0. find the bound workspace.
const bindings = (await inv("list_workspace_runtime_bindings")) as Array<{ workspaceId: string; runtimeName: string; remotePath: string }>;
const bound = bindings.find((x) => x.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}`);
const localDir = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/aludra";

// 1. fire agent.send + capture its request_id by waiting for any session
//    to appear in the daemon's agent.list.
const sessionInfo = (await inv("create_session", { workspaceId: bound.workspaceId })) as { sessionId: string };
const helmorSessionId = sessionInfo.sessionId;
console.error(`✓ fresh helmor session ${helmorSessionId.slice(0, 8)}`);

const provider = JSON.stringify({
	customBaseUrl: "http://host.docker.internal:1235",
	customApiKey: "lm-studio",
	customModels: "google/gemma-4-26b-a4b",
});
await inv("update_app_settings", { settingsMap: { "app.claude_custom_providers": provider } });

const driver = `
	window.__taper = window.__taper || {};
	var slot = (window.__taper.dp = { evs: [], done: false, error: null });
	var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {
		if (raw && 'end' in raw) { slot.done = true; return; }
		slot.evs.push(raw && raw.message);
	});
	var ch = { toJSON: function(){ return "__CHANNEL__:" + id; } };
	var req = ${JSON.stringify({
		provider: "claude",
		modelId: "claude-custom|custom|google/gemma-4-26b-a4b",
		prompt: "Reply with exactly: REMOTE_AGENT_OK",
		sessionId: null,
		helmorSessionId,
		workingDirectory: localDir,
		effortLevel: "medium",
		permissionMode: "bypassPermissions",
		fastMode: false,
	})};
	var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", { request: req, onEvent: ch });
	p["then"](function(){}, function(e){ slot.error = String(e && e.message ? e.message : e); slot.done = true; });
	return "started";
`;
await b.executeJs(driver);
console.error("✓ agent.send fired");

// 2. List sessions; daemons take a tick to receive the request_id.
let sessions: Array<{ requestId: string; state?: string }> = [];
const listDeadline = Date.now() + 15_000;
while (Date.now() < listDeadline) {
	sessions = (await inv("list_remote_agent_sessions", { name: NAME })) as typeof sessions;
	if (sessions.length > 0) break;
	await Bun.sleep(400);
}
if (sessions.length === 0) {
	console.error("✗ no agent sessions registered with the daemon — agent.send didn't reach the bridge");
	process.exit(1);
}
const targetRequestId = sessions[sessions.length - 1].requestId;
console.error(`✓ daemon has ${sessions.length} session(s); newest request_id=${targetRequestId.slice(0, 8)}`);

// 3. snapshot daemon pid BEFORE the disconnect.
const pidBefore = await daemonPid();
if (!pidBefore) {
	console.error("✗ couldn't read daemon pid before disconnect");
	process.exit(1);
}
console.error(`✓ daemon pid before disconnect: ${pidBefore}`);

// 4. force disconnect — this kills the SSH proxy + the desktop's RpcClient.
await inv("disconnect_remote_runtime", { name: NAME }, 30_000);
await Bun.sleep(800);
console.error("✓ disconnected remote runtime");

// 5. reconnect — fresh SSH session, new --proxy, should bind to same socket.
await inv("connect_remote_runtime", {
	name: NAME,
	host: HOST,
	remoteBinary: BIN,
	forwardAgent: false,
}, 30_000);
console.error("✓ reconnected remote runtime");

// 6. snapshot daemon pid AFTER reconnect.
const pidAfter = await daemonPid();
const pidSame = pidAfter === pidBefore;
console.error(`✓ daemon pid after reconnect: ${pidAfter} (same as before: ${pidSame})`);

// 7. list again — session should still be there.
const sessionsAfter = (await inv("list_remote_agent_sessions", { name: NAME })) as Array<{ requestId: string; state?: string }>;
const stillPresent = sessionsAfter.some((s) => s.requestId === targetRequestId);
console.error(`✓ session still in daemon after reconnect: ${stillPresent} (${sessionsAfter.length} total)`);

b.close();
process.exit(pidSame && stillPresent ? 0 : 1);
