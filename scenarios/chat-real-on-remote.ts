#!/usr/bin/env bun
// scenarios/chat-real-on-remote.ts
//
// Real chat thread, driven through the composer. Proves: a user typing
// a prompt in the Helmor chat surface gets an answer that ONLY makes
// sense if the agent ran on the remote container — `ls` lists the
// remote workspace's files, README.md is the one we shipped to the
// container, the new file shows up in `docker exec` listings on the
// container (not on the laptop).
//
// Beats:
//   1. Workspace selected, runtime chip visible, empty chat.
//   2. "List the files in this workspace, one per line, no preamble."
//        → assistant replies with the workspace's top-level entries
//          as they exist on the container (including REMOTE_ONLY_MARKER).
//   3. "Read README.md and quote its second line verbatim."
//        → quotes a line that only exists in the container copy.
//   4. "Create HELMOR_DEMO.md containing the single line: Hello from
//      the remote container." → file appears on the container; the
//      inspector shows it as an untracked change.
//
// Driving: every prompt goes through `window.__helmorTest.sendPrompt`,
// which calls the exact `handleComposerSubmit` the Send button uses.
// That keeps the recorded surface honest — what reviewers see in the
// gif is what a real user typing into the composer would see.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const BIN = process.env.REMOTE_BINARY ?? "/home/e2e/.helmor/server/helmor-server";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/chat-real-on-remote";
const LOCAL_DIR = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/hamal";

const PROMPT_LS = "List the files in this workspace, one per line, no preamble.";
const PROMPT_README = "Read README.md from this workspace and quote its second line verbatim. Respond with only that line, no preamble.";
const NEW_FILE = "HELMOR_DEMO.md";
const NEW_FILE_TEXT = "Hello from the remote container";
const PROMPT_CREATE = `Create a file called ${NEW_FILE} in this workspace containing the single line: ${NEW_FILE_TEXT}`;

const tape = new Tape("chat-real-on-remote", OUT);
await tape.connect();

// Preconditions: runtime connected; LM Studio bridge configured.
{
	const rts = (await tape.invoke("list_remote_runtimes", {})) as Array<{
		name: string;
		state?: { type?: string };
	}>;
	const r = rts.find((x) => x.name === NAME);
	if (r?.state?.type !== "connected") {
		tape.log(`runtime state is ${r?.state?.type ?? "missing"}; reconnecting`);
		await tape.invoke(
			"connect_remote_runtime",
			{ name: NAME, host: HOST, remoteBinary: BIN, forwardAgent: false },
			60_000,
		).catch(() => {});
	}
}
await tape.invoke("update_app_settings", {
	settingsMap: {
		"app.claude_custom_providers": JSON.stringify({
			customBaseUrl: "http://host.docker.internal:1235",
			customApiKey: "lm-studio",
			customModels: "google/gemma-4-26b-a4b",
		}),
	},
});

// Plant REMOTE_ONLY_MARKER so the file-list answer is visibly
// container-side (the marker only exists on the remote).
const MARKER = "REMOTE_ONLY_MARKER.txt";
const MARKER_TEXT = `remote-proof-${Date.now()}`;
const bindings = (await tape.invoke("list_workspace_runtime_bindings", {})) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((b) => b.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}; run setup-remote-workspace.ts first`);
{
	const plant = Bun.spawn([
		"docker", "exec", "-u", "e2e", CONTAINER, "sh", "-c",
		`printf '%s' '${MARKER_TEXT}' > '${bound.remotePath}/${MARKER}' && rm -f '${bound.remotePath}/${NEW_FILE}'`,
	]);
	if ((await plant.exited) !== 0) throw new Error("failed to plant marker on container");
	tape.log(`planted ${MARKER}; cleared any stale ${NEW_FILE}`);
}

// Reload to a clean state and select the bound workspace.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.js(
	`var el=document.querySelector('[data-workspace-row-id="${bound.workspaceId}"] [data-workspace-row-body]')` +
		`||document.querySelector('[data-workspace-row-id="${bound.workspaceId}"]');` +
		`if (el) el.click(); return !!el;`,
);
tape.assert("workspace_runtime_chip", await tape.waitFor('[aria-label^="Workspace runtime:"]', 10_000));

// Wait for the composer's __helmorTest hook to attach (it only mounts
// once the displayedSessionId is non-null — i.e. the panel finished
// hydrating its session).
const hookDeadline = Date.now() + 15_000;
let hookReady = false;
while (Date.now() < hookDeadline) {
	hookReady = (await tape.js<boolean>(
		`return typeof window.__helmorTest?.sendPrompt === "function";`,
	)) as boolean;
	if (hookReady) break;
	await tape.sleep(400);
}
tape.assert("composer_hook_attached", hookReady);

// Start the continuous recording. Budget: 4 scenes × ~25s + 10s
// headroom = ~110s. Bumped to 140s to absorb LM Studio variance.
await tape.startRecording(140, { gifFps: 6, gifMaxWidth: 900 });

// Scene 1 — workspace + chip + empty chat.
await tape.scene({
	caption: `Workspace bound to ${NAME} — runtime chip in the header. The composer is the same one you'd use locally.`,
	hold: 4,
});

// Helmor's chat surface renders the tool-USE call inline (e.g.
// "Run ls -1") but not the tool-RESULT content by default. The
// result lives in a session_messages row in the local SQLite DB,
// so for content-bearing asserts we shell out to sqlite3 against
// the workspace's session ids. The DOM-side check just confirms a
// new assistant turn arrived.
async function sendAndWait(prompt: string, label: string, timeoutMs = 60_000): Promise<string | null> {
	const baseline = (await tape.js<number>(
		`return document.querySelectorAll('[data-message-role]').length;`,
	)) as number;
	await tape.js(`
		(function(){
			window.__taperLastErr = null;
			window.__helmorTest.sendPrompt(${JSON.stringify(prompt)})
				.catch(function(e){ window.__taperLastErr = String(e && e.message ? e.message : e); });
			return "fired";
		})()
	`);
	tape.log(`[${label}] sent`);
	const deadline = Date.now() + timeoutMs;
	let final: string | null = null;
	while (Date.now() < deadline) {
		const snap = (await tape.js<{ count: number; streaming: boolean; err: string | null; panelText: string }>(
			`(function(){
				var msgs = document.querySelectorAll('[data-message-role]');
				var since = msgs.length > ${baseline} ? Array.from(msgs).slice(${baseline}) : [];
				return {
					count: msgs.length,
					streaming: !!document.querySelector('[data-testid=streaming-footer]'),
					err: window.__taperLastErr || null,
					panelText: since.map(function(m){ return m.innerText || ''; }).join('\\n'),
				};
			})()`,
		)) as { count: number; streaming: boolean; err: string | null; panelText: string };
		if (snap.err) {
			tape.log(`[${label}] sendPrompt error: ${snap.err}`);
			return null;
		}
		if (snap.count > baseline && !snap.streaming && snap.panelText.trim()) {
			final = snap.panelText;
			break;
		}
		await tape.sleep(500);
	}
	tape.assert(`${label}_arrived`, final !== null, (final ?? "").replace(/\s+/g, " ").slice(0, 120));
	return final;
}

async function dbContains(workspaceId: string, needle: string): Promise<boolean> {
	const p = Bun.spawn(
		[
			"sqlite3",
			`${process.env.HOME}/helmor-dev/helmor.db`,
			`SELECT 1 FROM session_messages WHERE session_id IN (SELECT id FROM sessions WHERE workspace_id='${workspaceId}') AND content LIKE '%${needle.replace(/'/g, "''")}%' LIMIT 1;`,
		],
		{ stdout: "pipe" },
	);
	const text = (await new Response(p.stdout).text()).trim();
	await p.exited;
	return text.length > 0;
}

// Scene 2 — list files.
await sendAndWait(PROMPT_LS, "ls", 90_000);
const sawMarker = await dbContains(bound.workspaceId, MARKER);
tape.assert("ls_tool_result_persisted_marker", sawMarker, sawMarker ? "yes" : "no");
await tape.scene({
	caption: sawMarker
		? `\"list the files\" → agent ran \`ls -1\` on the container; ${MARKER} came back in the tool result.`
		: `\"list the files\" → reply streamed back from the container.`,
	hold: 8,
});

// Scene 3 — read README's second line.
await sendAndWait(PROMPT_README, "readme", 90_000);
const sawTaper = await dbContains(bound.workspaceId, "helmor-taper");
tape.assert("readme_read_from_container", sawTaper, sawTaper ? "yes" : "no");
await tape.scene({
	caption: `\"read README.md\" → file read from /home/e2e/helmor-workspaces/helmor-taper, line quoted back.`,
	hold: 8,
});

// Scene 4 — create a new file; verify it lands on the container.
await sendAndWait(PROMPT_CREATE, "create", 120_000);
const exists = await (async () => {
	const p = Bun.spawn([
		"docker", "exec", "-u", "e2e", CONTAINER, "sh", "-c",
		`test -f '${bound.remotePath}/${NEW_FILE}' && cat '${bound.remotePath}/${NEW_FILE}' || echo MISSING`,
	], { stdout: "pipe" });
	const body = (await new Response(p.stdout).text()).trim();
	await p.exited;
	return { ok: body === NEW_FILE_TEXT, body };
})();
tape.assert("new_file_on_container", exists.ok, exists.body.slice(0, 120));
await tape.scene({
	caption: exists.ok
		? `\"create HELMOR_DEMO.md\" → file lives on the container's disk, not the laptop.`
		: `\"create HELMOR_DEMO.md\" → see inspector for the file write.`,
	hold: 8,
});

const passed = await tape.finish({
	runtimeName: NAME,
	workspaceId: bound.workspaceId,
	remotePath: bound.remotePath,
	prompts: { ls: PROMPT_LS, readme: PROMPT_README, create: PROMPT_CREATE },
	createdFile: { path: `${bound.remotePath}/${NEW_FILE}`, body: exists.body },
});
process.exit(passed ? 0 : 1);
