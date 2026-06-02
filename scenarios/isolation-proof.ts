#!/usr/bin/env bun
// scenarios/isolation-proof.ts
//
// Three back-to-back chat exchanges that pin down what the agent CAN
// and CANNOT see. The whole point: a viewer who watches the gif comes
// away convinced that this agent is running on the container, not the
// laptop — because it answers with container facts when asked
// container questions, and reports "not found" for paths that only
// exist on the laptop.
//
// Beats:
//   1. Workspace selected, empty chat.
//   2. "Output ONLY the hostname." → container hostname (NOT the
//      laptop's hostname).
//   3. "Does /Users/david exist? Reply yes or no." → "no" (that's a
//      macOS path; the container's filesystem has no /Users).
//   4. "What's the absolute pwd? Reply with just the path." → echoes
//      /home/e2e/helmor-workspaces/... (NOT a laptop path).

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const BIN = process.env.REMOTE_BINARY ?? "/home/e2e/.helmor/server/helmor-server";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/isolation-proof";

const PROMPT_HOSTNAME = "Output ONLY the hostname of the machine you're running on. No other text, no preamble.";
const PROMPT_USERS = "Does the path /Users/david exist on the filesystem you're operating on? Reply with only 'yes' or 'no'.";
const PROMPT_PWD = "What's the absolute path of your current working directory? Reply with only the path, nothing else.";

const tape = new Tape("isolation-proof", OUT);
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

const bindings = (await tape.invoke("list_workspace_runtime_bindings", {})) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((b) => b.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}; run setup-remote-workspace.ts first`);

// Capture the container's REAL hostname so we can assert against the
// reply. `docker exec hostname` is the ground truth for this gif.
const containerHostname = await (async () => {
	const p = Bun.spawn(["docker", "exec", CONTAINER, "hostname"], { stdout: "pipe" });
	const out = (await new Response(p.stdout).text()).trim();
	await p.exited;
	return out;
})();
const laptopHostname = await (async () => {
	const p = Bun.spawn(["hostname"], { stdout: "pipe" });
	const out = (await new Response(p.stdout).text()).trim();
	await p.exited;
	return out;
})();
tape.log(`container hostname=${containerHostname}, laptop hostname=${laptopHostname}`);

// Reload + select bound workspace + wait for composer hook.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.js(
	`var el=document.querySelector('[data-workspace-row-id="${bound.workspaceId}"] [data-workspace-row-body]')` +
		`||document.querySelector('[data-workspace-row-id="${bound.workspaceId}"]');` +
		`if (el) el.click(); return !!el;`,
);
tape.assert("workspace_runtime_chip", await tape.waitFor('[aria-label^="Workspace runtime:"]', 10_000));
{
	const deadline = Date.now() + 15_000;
	let ready = false;
	while (Date.now() < deadline) {
		ready = (await tape.js<boolean>(
			`return typeof window.__helmorTest?.sendPrompt === "function";`,
		)) as boolean;
		if (ready) break;
		await tape.sleep(400);
	}
	tape.assert("composer_hook_attached", ready);
}

// 3 scenes × ~25s each + 10s headroom = ~85s. Allow LM Studio
// variance + closing scene.
await tape.startRecording(120, { gifFps: 6, gifMaxWidth: 900 });

await tape.scene({
	caption: `The agent below runs in a Docker container; the laptop is just the viewport.`,
	hold: 4,
});

// The chat surface renders the tool-USE call inline but not the
// tool-RESULT body; content-bearing asserts query the DB.
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

async function dbContainsRecent(workspaceId: string, needle: string, sinceMs: number): Promise<boolean> {
	const cutoff = new Date(sinceMs).toISOString();
	const p = Bun.spawn(
		[
			"sqlite3",
			`${process.env.HOME}/helmor-dev/helmor.db`,
			`SELECT 1 FROM session_messages WHERE session_id IN (SELECT id FROM sessions WHERE workspace_id='${workspaceId}') AND created_at > '${cutoff}' AND content LIKE '%${needle.replace(/'/g, "''")}%' LIMIT 1;`,
		],
		{ stdout: "pipe" },
	);
	const text = (await new Response(p.stdout).text()).trim();
	await p.exited;
	return text.length > 0;
}

// Beat 2: hostname.
const t1 = Date.now();
await sendAndWait(PROMPT_HOSTNAME, "hostname", 90_000);
const sawContainerHost = await dbContainsRecent(bound.workspaceId, containerHostname, t1);
const sawLaptopHost = laptopHostname && laptopHostname.length > 3
	? await dbContainsRecent(bound.workspaceId, laptopHostname, t1)
	: false;
tape.assert(
	"hostname_is_container_not_laptop",
	sawContainerHost && !sawLaptopHost,
	`container_seen=${sawContainerHost}, laptop_seen=${sawLaptopHost} (container=${containerHostname}, laptop=${laptopHostname})`,
);
await tape.scene({
	caption: `\"hostname\" → ${containerHostname} (container). Laptop's hostname doesn't appear anywhere.`,
	hold: 8,
});

// Beat 3: laptop path absence.
const t2 = Date.now();
await sendAndWait(PROMPT_USERS, "users_path", 90_000);
// Look for a tool_result whose content was literally "no" (the
// expected exit-code-driven reply). Bounded substring keeps it
// false on unrelated rows.
const sawNo = await dbContainsRecent(bound.workspaceId, '"content":"no"', t2);
tape.assert("users_path_reported_absent", sawNo, sawNo ? "yes" : "no");
await tape.scene({
	caption: `\"/Users/david exist?\" → no. The container's filesystem has no /Users tree at all.`,
	hold: 8,
});

// Beat 4: pwd is on the container.
const t3 = Date.now();
await sendAndWait(PROMPT_PWD, "pwd", 90_000);
const onContainerPath = await dbContainsRecent(bound.workspaceId, "/home/e2e/", t3);
tape.assert("pwd_on_container_path", onContainerPath, onContainerPath ? "yes" : "no");
await tape.scene({
	caption: `\"pwd\" → /home/e2e/... — the agent's CWD lives on the container, not the laptop.`,
	hold: 8,
});

const passed = await tape.finish({
	runtimeName: NAME,
	workspaceId: bound.workspaceId,
	remotePath: bound.remotePath,
	hostnames: { container: containerHostname, laptop: laptopHostname },
});
process.exit(passed ? 0 : 1);
