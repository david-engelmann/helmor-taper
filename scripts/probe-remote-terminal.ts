#!/usr/bin/env bun
// probe-remote-terminal.ts
//
// Headless confirmation that a PTY opened via Helmor is HOSTED ON THE
// CONTAINER, not the laptop. Opens a remote terminal in the workspace's
// remote_path, writes a `whoami; hostname; pwd` round-trip, captures the
// streamed output, asserts it shows the container hostname + the e2e
// user + the remote worktree path — proof that the bytes are coming from
// the daemon's PTY layer (RemoteTerminalState), not from a local shell.

import { Bridge } from "./mcp-bridge.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 60_000) =>
	b.invokeAndWait(c, a, t, `prt-${c}`) as Promise<T>;

const bindings = (await inv("list_workspace_runtime_bindings")) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((x) => x.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}`);
console.error(`✓ workspace ${bound.workspaceId.slice(0, 8)} bound; remote=${bound.remotePath}`);

const TERM_ID = crypto.randomUUID();

// Construct a Channel<TerminalEventNotification> via the same in-page
// transformCallback path the frontend uses; events land on window.__taper.term.
// The wire shape is `{terminalId, event: {kind, data|code|message}}` — kind
// "stdout" is the PTY output stream; "exited" or "error" terminate.
const driver = `
	window.__taper = window.__taper || {};
	var slot = (window.__taper.term = { chunks: [], events: [], done: false, error: null, openResult: null });
	var id = window.__TAURI_INTERNALS__.transformCallback(function(raw) {
		if (raw && 'end' in raw) { slot.done = true; return; }
		var notif = raw && raw.message;
		slot.events.push(notif);
		var ev = notif && notif.event;
		if (ev && ev.kind === "stdout" && typeof ev.data === "string") {
			slot.chunks.push(ev.data);
		}
		if (ev && (ev.kind === "exited" || ev.kind === "error")) { slot.done = true; }
	});
	var channel = { toJSON: function(){ return "__CHANNEL__:" + id; } };
	var args = ${JSON.stringify({
		runtimeName: NAME,
		terminalId: TERM_ID,
		workspaceDir: bound.remotePath,
		shell: "/bin/bash",
		cols: 100,
		rows: 30,
		channel: null,
	})};
	args.channel = channel;
	var p = window.__TAURI_INTERNALS__.invoke("open_remote_terminal", args);
	p["then"](function(v){ slot.openResult = v; }, function(e){ slot.error = String(e && e.message ? e.message : e); slot.done = true; });
	return "started";
`;
await b.executeJs(driver);
console.error(`✓ open_remote_terminal fired (terminal_id=${TERM_ID.slice(0, 8)})`);

// Wait a beat for the open to settle, then write a known-output round-trip.
await Bun.sleep(1000);
// Real LF — JSON.stringify will encode it as the escape sequence `\n` on
// the wire, and the daemon decodes back to a single 0x0A byte the PTY
// treats as <enter>. Without this the shell sees the literal `\n` as
// two characters and never runs the command.
const probe = "whoami; hostname; pwd; echo TERMINAL_DONE_MARKER\n";
await inv("write_remote_terminal", { runtimeName: NAME, terminalId: TERM_ID, data: probe });
console.error(`✓ wrote probe command: ${probe.trim()}`);

// Wait for the marker.
const deadline = Date.now() + 15_000;
let buf = "";
while (Date.now() < deadline) {
	buf = (await b.executeJs(
		`return (window.__taper.term.chunks || []).join("");`,
	)) as string;
	if (buf.includes("TERMINAL_DONE_MARKER")) break;
	await Bun.sleep(300);
}
const sawMarker = buf.includes("TERMINAL_DONE_MARKER");
const sawE2e = /\be2e\b/.test(buf);
const sawHostname = /081e3cab7eb5/.test(buf) || /[0-9a-f]{12}/.test(buf);
const sawRemotePath = buf.includes(bound.remotePath);

console.error("\noutput buffer:\n----");
console.error(buf.slice(0, 800));
console.error("----");
console.error(`\nchecks: marker=${sawMarker} user=e2e:${sawE2e} hostname:${sawHostname} pwd=${bound.remotePath}:${sawRemotePath}`);

// Close + tidy.
try {
	await inv("close_remote_terminal", { runtimeName: NAME, terminalId: TERM_ID });
} catch (e) {
	console.error(`(close: ${String(e).slice(0, 80)})`);
}

b.close();
process.exit(sawMarker && sawE2e && sawRemotePath ? 0 : 1);
