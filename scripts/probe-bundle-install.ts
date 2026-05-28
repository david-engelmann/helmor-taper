#!/usr/bin/env bun
// probe-bundle-install.ts
//
// End-to-end test of the productionized install path: a host with only
// the bare `helmor-server` daemon binary (no sidecar, no claude, no
// wrapper) gets the full agent-runtime bundle pushed via the
// `install_remote_bundle` Tauri command, atomic + sha256-verified, and
// then a fresh `agent.send` actually runs the model on the container
// and streams `REMOTE_AGENT_OK` back. No `docker cp`, no `npm pack`,
// no manual wrapper writing — every byte arrives via the install
// pipeline, exactly the way a real operator's first connect would.
//
// Preconditions (set up by the runner): the container is up, the
// container has `helmor-server` (the daemon binary) but NOT the
// sidecar/claude/wrapper, and Helmor's `bun run dev` is alive with
// the runtime already connected.

import { Bridge } from "./mcp-bridge.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 120_000) =>
	b.invokeAndWait(c, a, t, `pbi-${c}`) as Promise<T>;

// 0. Inspect the pre-install state on the container — should be
//    "daemon only, no bundle artifacts."
async function dockerLs(): Promise<string> {
	const p = Bun.spawn(
		["docker", "exec", "-u", "e2e", CONTAINER, "sh", "-c", "ls $HOME/.helmor/server/ 2>/dev/null | sort"],
		{ stdout: "pipe" },
	);
	return (await new Response(p.stdout).text()).trim();
}
const before = await dockerLs();
console.error(`=== pre-install state ===\n${before}\n=========================`);

// 1. Drive the install. The full bundle is ~330 MB (110 MB sidecar +
//    220 MB claude); over a real LAN scp this is 1–2 minutes, so the
//    timeout is generous. Subsequent runs are no-ops in milliseconds.
const outcome = (await inv("install_remote_bundle", { name: NAME }, 600_000)) as {
	manifest: { target: string; claudeCodeVersion: string; files: Array<{ path: string; sha256: string; bytes: number }> };
	installedFiles: string[];
	alreadyCurrent: boolean;
};
console.error(`✓ install_remote_bundle returned: target=${outcome.manifest.target} claudeCode=${outcome.manifest.claudeCodeVersion}`);
console.error(`  installed files: ${outcome.installedFiles.join(", ") || "(none — already current)"}`);
console.error(`  alreadyCurrent: ${outcome.alreadyCurrent}`);

// 2. Verify the on-disk state matches the manifest.
const after = await dockerLs();
console.error(`=== post-install state ===\n${after}\n==========================`);
const expectedFiles = ["MANIFEST.json", "claude", "helmor-server", "helmor-server.real", "helmor-sidecar"];
const seen = new Set(after.split(/\s+/).filter(Boolean));
const missing = expectedFiles.filter((f) => !seen.has(f));
if (missing.length > 0) {
	console.error(`✗ missing expected post-install files: ${missing.join(", ")}`);
	process.exit(1);
}
console.error("✓ all expected files present on remote");

// 3. Verify a sha256 on the remote matches the manifest entry — this
//    is what install_bundle already does internally, but proving it
//    externally is the strongest possible evidence.
const claudeEntry = outcome.manifest.files.find((f) => f.path === "claude");
if (!claudeEntry) throw new Error("manifest missing 'claude' entry");
const shaProc = Bun.spawn(
	["docker", "exec", "-u", "e2e", CONTAINER, "sh", "-c", "sha256sum $HOME/.helmor/server/claude | cut -d' ' -f1"],
	{ stdout: "pipe" },
);
const observedSha = (await new Response(shaProc.stdout).text()).trim();
if (observedSha !== claudeEntry.sha256) {
	console.error(`✗ sha256 mismatch for claude: expected ${claudeEntry.sha256}, got ${observedSha}`);
	process.exit(1);
}
console.error(`✓ sha256(claude) on remote matches manifest (${observedSha.slice(0, 12)}…)`);

// 4. Re-run install — should be a no-op (alreadyCurrent: true).
const reRun = (await inv("install_remote_bundle", { name: NAME })) as {
	installedFiles: string[];
	alreadyCurrent: boolean;
};
if (!reRun.alreadyCurrent || reRun.installedFiles.length > 0) {
	console.error(`✗ second install should have been a no-op; got installedFiles=${JSON.stringify(reRun.installedFiles)}`);
	process.exit(1);
}
console.error("✓ second install is a no-op (idempotent)");

// 5. Reconnect so the now-installed wrapper actually drives the
//    persistent-daemon pipeline (the in-flight SSH session was using
//    the bare binary in ServeStdio mode — fine for RPC, but it
//    bypasses HELMOR_SIDECAR_PATH because the wrapper isn't sourced).
await inv("disconnect_remote_runtime", { name: NAME }).catch(() => {});
await Bun.sleep(800);
await inv("connect_remote_runtime", {
	name: NAME,
	host: "helmor-taper-arm64",
	remoteBinary: "/home/e2e/.helmor/server/helmor-server",
	forwardAgent: false,
}, 60_000);
console.error("✓ reconnected with the new wrapper in place");

// 6. Fire an agent.send on the bound workspace. This is the holy-
//    grail check: model output arrives from a sidecar the install
//    pipeline put on the container.
const bindings = (await inv("list_workspace_runtime_bindings")) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((x) => x.runtimeName === NAME);
if (!bound) {
	console.error("(no workspace bound to the runtime — skipping agent.send check)");
	console.error("✓ install path verified; agent.send check skipped");
	b.close();
	process.exit(0);
}

// Make sure the custom provider is wired to LM Studio.
await inv("update_app_settings", {
	settingsMap: {
		"app.claude_custom_providers": JSON.stringify({
			customBaseUrl: "http://host.docker.internal:1235",
			customApiKey: "lm-studio",
			customModels: "google/gemma-4-26b-a4b",
		}),
	},
});

const session = (await inv("create_session", { workspaceId: bound.workspaceId })) as { sessionId: string };
const driver = `
	window.__taper = window.__taper || {};
	var slot = (window.__taper.pbi = { evs: [], done: false, error: null });
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
		helmorSessionId: session.sessionId,
		workingDirectory: process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/aludra",
		effortLevel: "medium",
		permissionMode: "bypassPermissions",
		fastMode: false,
	})};
	var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", { request: req, onEvent: ch });
	p["then"](function(){}, function(e){ slot.error = String(e && e.message ? e.message : e); slot.done = true; });
	return "started";
`;
await b.executeJs(driver);

// Wait for the marker text to appear in any streamed assistant chunk.
const deadline = Date.now() + 90_000;
let sawMarker = false;
while (Date.now() < deadline) {
	const flat = (await b.executeJs(
		`var evs=(window.__taper.pbi||{}).evs||[]; return JSON.stringify(evs);`,
	)) as string;
	if (flat.includes("REMOTE_AGENT_OK")) {
		sawMarker = true;
		break;
	}
	await Bun.sleep(400);
}
console.error(`✓ agent response contains REMOTE_AGENT_OK: ${sawMarker}`);

b.close();
process.exit(sawMarker ? 0 : 1);
