#!/usr/bin/env bun
// probe-remote-agent.ts
//
// Headless confirmation that an agent.send to a workspace bound to the
// dockerized Linux runtime actually spawns claude-code ON THE CONTAINER and
// streams events back. Configures a custom Claude provider that targets the
// host's LM Studio (Anthropic-compatible endpoint), reaches it from the
// container via host.docker.internal, fires send_agent_message_stream over
// the MCP bridge with a hand-rolled Channel, and prints what the daemon
// emits. A green run proves: Linux sidecar runs ✓ claude binary runs ✓
// LM Studio reachable from the container ✓ remote_path translation for
// cwd ✓ end-to-end event streaming.

import { Bridge } from "./mcp-bridge.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const MODEL = process.env.MODEL ?? "google/gemma-4-26b-a4b";
const BASE_URL = process.env.LM_STUDIO_BASE ?? "http://host.docker.internal:1235";
const API_KEY = process.env.LM_STUDIO_KEY ?? "lm-studio";
const PROMPT = process.env.PROMPT ?? "Reply with exactly: REMOTE_AGENT_OK";

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 60_000) =>
	b.invokeAndWait(c, a, t, `probe-${c}`) as Promise<T>;

// 1. Wire the custom Claude provider to LM Studio. The setting body is a
//    JSON string; the rust side parses it on every send via load_setting_json.
const providerJson = JSON.stringify({
	customBaseUrl: BASE_URL,
	customApiKey: API_KEY,
	customModels: MODEL,
});
await inv("update_app_settings", { settingsMap: { "app.claude_custom_providers": providerJson } });
console.error(`✓ provider set: ${BASE_URL} · ${MODEL}`);

// 2. Find the workspace bound to the remote + its active session.
const bindings = (await inv("list_workspace_runtime_bindings")) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((x) => x.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}`);
const sessions = (await inv("list_workspace_sessions", { workspaceId: bound.workspaceId })) as Array<{
	id: string;
	active: boolean;
}>;
const session = sessions.find((s) => s.active) ?? sessions[0];
if (!session) throw new Error("no session in bound workspace");
console.error(`✓ ws=${bound.workspaceId.slice(0, 8)} session=${session.id.slice(0, 8)} remote=${bound.remotePath}`);

const localDir = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/albiorix";

// 3. Construct an AgentSendRequest. The provider for a custom-provider model
//    resolves to "claude" (see catalog.rs); model id is `claude-custom|custom|<model>`.
const modelId = `claude-custom|custom|${MODEL}`;
const request = {
	provider: "claude",
	modelId,
	prompt: PROMPT,
	sessionId: null,
	helmorSessionId: session.id,
	workingDirectory: localDir,
	effortLevel: "medium",
	permissionMode: "bypassPermissions",
	fastMode: false,
};

// 4. Drive send_agent_message_stream via the bridge. The frontend creates a
//    `Channel` from @tauri-apps/api/core; that class's wire form is just an
//    object whose toJSON()/SERIALIZE_TO_IPC_FN returns `__CHANNEL__:<id>`,
//    where the id comes from `__TAURI_INTERNALS__.transformCallback(fn)`. We
//    replicate that here so each event the daemon emits lands on
//    `window.__taper.evs`. (`p["then"](...)` keeps the script syntactically
//    sync so the bridge takes its fast native path.)
const driver = `
	window.__taper = window.__taper || {};
	window.__taper.evs = [];
	window.__taper.done = false;
	window.__taper.error = null;
	var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {
		if (raw && 'end' in raw) { window.__taper.done = true; return; }
		window.__taper.evs.push(raw && raw.message);
	});
	var onEvent = { toJSON: function(){ return "__CHANNEL__:" + id; } };
	var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", { request: ${JSON.stringify(request)}, onEvent: onEvent });
	p["then"](function(){}, function(e){ window.__taper.error = String((e && e.message) ? e.message : e); window.__taper.done = true; });
	return "started";
`;
await b.executeJs(driver);
console.error(`✓ agent.send fired (model=${modelId})`);

// 5. Poll the channel buffer until done or timeout.
const deadline = Date.now() + 90_000;
let lastShown = 0;
while (Date.now() < deadline) {
	const s = (await b.executeJs(
		`var t=window.__taper||{}; return { defined: !!window.__taper, n: (t.evs||[]).length, done: !!t.done, error: t.error||null };`,
	)) as { defined: boolean; n: number; done: boolean; error: string | null };
	if (!s.defined) {
		console.error("✗ window.__taper vanished (page reloaded?) — aborting");
		break;
	}
	if (s.n > lastShown) {
		const fresh = (await b.executeJs(
			`var t=window.__taper||{}; return (t.evs||[]).slice(${lastShown});`,
		)) as Array<{ kind?: string; data?: { type?: string; text?: string } }>;
		for (const e of fresh) {
			console.error(`  · ${e.kind} ${JSON.stringify(e.data).slice(0, 180)}`);
		}
		lastShown = s.n;
	}
	if (s.error) {
		console.error(`✗ agent.send rejected: ${s.error}`);
		process.exit(1);
	}
	if (s.done) break;
	await Bun.sleep(500);
}
const total = (await b.executeJs(`var t=window.__taper||{}; return (t.evs||[]).length;`)) as number;
console.error(`\nfinal: ${total} events streamed back`);
b.close();
