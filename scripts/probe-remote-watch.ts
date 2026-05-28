#!/usr/bin/env bun
// probe-remote-watch.ts
//
// Headless confirmation that the file watcher routes onto the remote
// runtime: subscribes to UI mutation events, starts a workspace watch
// that the binding routes to the daemon's `workspace.startWatch`, plants
// a file inside the container's worktree with `docker exec`, and asserts
// the desktop sees a `WorkspaceFilesChanged` event for that workspace.
// If the watcher were still walking the local worktree it would never
// notice a container-side change.

import { Bridge } from "./mcp-bridge.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 60_000) =>
	b.invokeAndWait(c, a, t, `prw-${c}`) as Promise<T>;

const bindings = (await inv("list_workspace_runtime_bindings")) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((x) => x.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}`);
const localDir = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/aludra";
console.error(`✓ workspace ${bound.workspaceId.slice(0, 8)} → ${bound.remotePath}`);

// Subscribe to UI mutation events via a hand-rolled Channel. The desktop
// emits `WorkspaceFilesChanged { workspaceId }` for every watcher batch —
// local or remote — so seeing it tagged with our bound workspace id proves
// the wire-watch fired. `subscribe_ui_mutations` takes a `subscriptionId`
// the caller chooses; we pick a UUID so we don't collide with any real
// frontend subscriber the running app already has.
const SUB_ID = crypto.randomUUID();
const subscribe = `
	window.__taper = window.__taper || {};
	var slot = (window.__taper.watch = { events: [], done: false, error: null, subscriptionId: ${JSON.stringify(SUB_ID)} });
	var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {
		if (raw && 'end' in raw) { slot.done = true; return; }
		slot.events.push(raw && raw.message);
	});
	var ch = { toJSON: function(){ return "__CHANNEL__:" + id; } };
	var p = window.__TAURI_INTERNALS__.invoke("subscribe_ui_mutations", { subscriptionId: ${JSON.stringify(SUB_ID)}, onEvent: ch });
	p["then"](function(){}, function(e){ slot.error = String(e && e.message ? e.message : e); });
	return "subscribed";
`;
await b.executeJs(subscribe);
await Bun.sleep(400);
console.error("✓ subscribed to ui-mutations channel");

// Stop any prior watch (idempotent), then start a fresh one on the bound
// workspace. The bindings layer routes this to the daemon.
await inv("stop_workspace_watch", { workspaceId: bound.workspaceId }).catch(() => {});
const startResult = (await inv("start_workspace_watch", {
	workspaceId: bound.workspaceId,
	workspaceDir: localDir,
})) as { workspaceId: string; kind: string };
console.error(`✓ start_workspace_watch → kind=${startResult.kind}`);
if (startResult.kind !== "remote") {
	console.error(`✗ expected remote watcher, got ${startResult.kind}`);
	process.exit(1);
}

// Plant a file inside the CONTAINER's worktree. The daemon's watcher
// should pick it up and fire a notification → desktop receives
// WorkspaceFilesChanged on our channel.
const MARKER = `WATCHER_PROOF_${Date.now()}.txt`;
const plant = Bun.spawn([
	"docker", "exec", CONTAINER, "sh", "-c",
	`printf 'remote-watcher-proof' > '${bound.remotePath}/${MARKER}'`,
]);
await plant.exited;
console.error(`✓ planted ${MARKER} on container`);

// Wait for the event to bubble through.
const deadline = Date.now() + 15_000;
let saw = false;
let received: unknown[] = [];
while (Date.now() < deadline) {
	received = (await b.executeJs(
		`return (window.__taper.watch.events || []);`,
	)) as unknown[];
	saw = received.some((e) => {
		// UiMutationEvent serializes with `#[serde(tag = "type", …)]` —
		// see ui_sync/events.rs. The variant is `WorkspaceFilesChanged`,
		// so the wire form is `{type: "workspaceFilesChanged", workspaceId}`.
		const ev = e as { type?: string; workspaceId?: string };
		return (
			ev.type === "workspaceFilesChanged" &&
			ev.workspaceId === bound.workspaceId
		);
	});
	if (saw) break;
	await Bun.sleep(400);
}
console.error(`\nfilewatch event observed: ${saw}`);
if (!saw) {
	console.error("recent events:");
	for (const ev of received.slice(-10)) console.error("  ·", JSON.stringify(ev).slice(0, 180));
}

// Cleanup: drop the watch + the subscription.
try {
	await inv("stop_workspace_watch", { workspaceId: bound.workspaceId });
} catch (e) {
	console.error(`(stop: ${String(e).slice(0, 80)})`);
}
try {
	await inv("unsubscribe_ui_mutations", { subscriptionId: SUB_ID });
} catch (e) {
	console.error(`(unsubscribe: ${String(e).slice(0, 80)})`);
}
b.close();
process.exit(saw ? 0 : 1);
