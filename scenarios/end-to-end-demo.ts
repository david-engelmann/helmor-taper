#!/usr/bin/env bun
// scenarios/end-to-end-demo.ts
//
// THE demo. One scenario, one master.gif, walks a reviewer through
// the full user journey for the remote-runner feature so a project
// maintainer can grasp the whole thing in 75–90 seconds without
// reading any code.
//
// Beats:
//   1.  Remote Servers panel — connected runtime baseline
//   2.  Reinstall click → install chip enters "detecting"
//   3.  Chip: "Uploading agent runtime (… MB)"
//   4.  Chip green: "Agent runtime installed in N.Ns"
//   5.  Workspace bound to the remote — header + sidebar chip
//   6.  Runtime Debug → Workspace inspector probe → file tree
//       result lists files from the container (incl. a planted
//       REMOTE_ONLY_MARKER)
//   7.  Inspector probe → Run changes → marker appears as
//       untracked on the container
//   8.  Runtime Debug → Remote agent sessions: agent.send fires
//       → row appears with provider/workspace/last-event timestamps
//   9.  Header banner area, container alive — "all green"
//   10. `docker stop` → banner flips to "Degraded — docker-…"
//   11. `docker start` → click Reconnect on the banner
//   12. Banner clears, runtime back to Connected
//   13. Final card: managed dir on the remote + the uninstall recipe
//
// To produce a clean reviewable artifact this run:
//   - WIPES the container's bundle so the install chip shows real
//     transitions (no "alreadyCurrent" no-op).
//   - PLANTS a `REMOTE_ONLY_MARKER` on the container so the
//     file-ops scene has a deterministic proof point.
//   - LEAVES the container in a healthy state at the end.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const BIN = process.env.REMOTE_BINARY ?? "/home/e2e/.helmor/server/helmor-server";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/end-to-end-demo";

const tape = new Tape("end-to-end-demo", OUT);
await tape.connect();

// ── Preconditions ──────────────────────────────────────────────────
// Make sure the runtime is connected (recover from a previous tape's
// drop) so we land on beat 1 with a green row.
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

// Discover the bound workspace so the agent.send + workspace-chip
// beats target a real session.
const bindings = (await tape.invoke("list_workspace_runtime_bindings", {})) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((b) => b.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}; run setup-remote-workspace.ts first`);
const localDir = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/aludra";
tape.log(`bound workspace: ${bound.workspaceId.slice(0, 8)} → ${bound.remotePath}`);

// Wipe the container's bundle so beat 3 shows a real upload.
{
	const wipe = Bun.spawn([
		"docker", "exec", "-u", "e2e", CONTAINER, "sh", "-c",
		"rm -f $HOME/.helmor/server/helmor-sidecar; " +
			"rm -f $HOME/.helmor/server/claude; " +
			"rm -f $HOME/.helmor/server/MANIFEST.json; " +
			"rm -rf $HOME/.helmor/server/.staging; " +
			"if [ -f $HOME/.helmor/server/helmor-server.real ]; then " +
			"  mv -f $HOME/.helmor/server/helmor-server.real $HOME/.helmor/server/helmor-server; " +
			"fi",
	]);
	if ((await wipe.exited) !== 0) throw new Error("failed to wipe container bundle");
	tape.log("wiped container bundle artifacts");
}

// Plant the REMOTE_ONLY_MARKER for the file-ops beat.
const MARKER = "REMOTE_ONLY_MARKER.txt";
const MARKER_TEXT = `remote-proof-${Date.now()}`;
{
	const plant = Bun.spawn([
		"docker", "exec", "-u", "e2e", CONTAINER, "sh", "-c",
		`printf '%s' '${MARKER_TEXT}' > '${bound.remotePath}/${MARKER}'`,
	]);
	if ((await plant.exited) !== 0) throw new Error("failed to plant marker on container");
	tape.log(`planted ${MARKER} on container`);
}

// Configure the LM Studio bridge so the agent.send beat actually
// produces a response.
await tape.invoke("update_app_settings", {
	settingsMap: {
		"app.claude_custom_providers": JSON.stringify({
			customBaseUrl: "http://host.docker.internal:1235",
			customApiKey: "lm-studio",
			customModels: "google/gemma-4-26b-a4b",
		}),
	},
});

// Reload to a clean shell.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);

// ── Beat 1 — connected baseline ────────────────────────────────────
await tape.openSettings("remote-servers");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]", 10_000));
tape.assert(
	"row_present",
	await tape.waitFor(`[data-testid=remote-server-row-${NAME}]`, 10_000),
);
await tape.scene({
	caption: `Helmor — connected to ${NAME}, no agent runtime yet`,
	hold: 5,
});

// ── Beats 2–4 — install chip transitions ───────────────────────────
await tape.click(`[data-testid=remote-server-reinstall-bundle-${NAME}]`);
tape.assert(
	"installing_chip",
	await tape.waitFor(
		`[data-testid=remote-server-bundle-installing-${NAME}]`,
		10_000,
	),
);
await tape.scene({
	caption: "Reinstall → sha256-verified tar-stream over SSH, atomic per-file",
	record: 3,
	hold: 5,
});

await tape.sleep(2000); // let the "Uploading…" phase land in the chip
await tape.scene({
	caption: "Everything lands in $HOME/.helmor/server/ — no sudo, no shell rc edits",
	record: 3,
	hold: 5,
});

tape.assert(
	"installed_chip",
	await tape.waitFor(
		`[data-testid=remote-server-bundle-installed-${NAME}]`,
		60_000,
	),
);
const chipText = await tape.js<string | null>(
	`var c=document.querySelector('[data-testid=remote-server-bundle-installed-${NAME}]'); return c?c.innerText:null;`,
);
tape.log(`install chip: ${chipText}`);
await tape.scene({
	caption: chipText ? `${chipText} · ready to run agents on the container` : "Agent runtime installed",
	hold: 5,
});

// ── Beat 5 — workspace bound to remote ─────────────────────────────
await tape.closeDialog();
await tape.sleep(500);
// Select the bound workspace so the runtime chip appears in the
// header + sidebar row.
await tape.js(
	`var el=document.querySelector('[data-workspace-row-id="${bound.workspaceId}"] [data-workspace-row-body]')` +
		`||document.querySelector('[data-workspace-row-id="${bound.workspaceId}"]');` +
		`if (el) el.click(); return !!el;`,
);
await tape.waitFor('[aria-label^="Workspace runtime:"]', 10_000);
await tape.scene({
	caption: `Workspace bound to ${NAME} — the blue chip says "files live in the container"`,
	hold: 5,
});

// ── Beats 6–7 — file ops on the container ──────────────────────────
await tape.openSettings("runtime-debug");
tape.assert("debug_panel_opens", await tape.waitFor("[role=dialog]", 10_000));
// Scroll the Workspace inspector probe section into view.
await tape.js(
	`var el=document.querySelector('#inspector-probe-workspace'); ` +
		`if (!el) return false; ` +
		`(el.closest('section')||el).scrollIntoView({block:'start',behavior:'auto'}); return true;`,
);
await tape.sleep(400);
// Fill workspace id + dir so the auto-via-binding lookup routes the
// call onto docker-linux-arm64.
const setInputJs = (sel: string, value: string) =>
	tape.js(
		`var el=document.querySelector(${JSON.stringify(sel)}); ` +
			`if (!el) return "no-input"; ` +
			`var d=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value'); ` +
			`(d && d.set) ? d.set.call(el, ${JSON.stringify(value)}) : (el.value=${JSON.stringify(value)}); ` +
			`el.dispatchEvent(new Event('input',{bubbles:true})); return "ok";`,
	);
await setInputJs("#inspector-probe-workspace-id", bound.workspaceId);
await setInputJs("#inspector-probe-workspace", localDir);
await tape.sleep(300);
// Click Run file tree.
await tape.js(
	`var bs=document.querySelectorAll('button'); ` +
		`for(var i=0;i<bs.length;i++){ if((bs[i].innerText||'').trim()==='Run file tree'){ bs[i].click(); return true; } } return false;`,
);
// Wait for results.
await (async () => {
	const deadline = Date.now() + 10_000;
	while (Date.now() < deadline) {
		const hit = await tape.js<boolean>(
			`return ((document.querySelector('[role=dialog]')||{}).innerText||'').indexOf('files (showing first') >= 0;`,
		);
		if (hit) return;
		await tape.sleep(300);
	}
})();
await tape.scene({
	caption: `File tree → entries come from ${bound.remotePath} on the container`,
	hold: 6,
});

// Run changes → marker appears.
await tape.js(
	`var bs=document.querySelectorAll('button'); ` +
		`for(var i=0;i<bs.length;i++){ if((bs[i].innerText||'').trim()==='Run changes'){ bs[i].click(); return true; } } return false;`,
);
await (async () => {
	const deadline = Date.now() + 10_000;
	while (Date.now() < deadline) {
		const hit = await tape.js<boolean>(
			`return ((document.querySelector('[role=dialog]')||{}).innerText||'').indexOf(${JSON.stringify(MARKER)}) >= 0;`,
		);
		if (hit) return;
		await tape.sleep(300);
	}
})();
await tape.scene({
	caption: `Planted a file via docker exec → Run changes lists it. Proof: container, not laptop.`,
	hold: 6,
});

// ── Beat 8 — agent runs in the container ───────────────────────────
// Scroll to the Remote agent sessions section.
await tape.js(
	`var hs=document.querySelectorAll('h3'); ` +
		`for(var i=0;i<hs.length;i++){ if(/Remote agent sessions/i.test(hs[i].textContent||'')){ (hs[i].closest('section')||hs[i]).scrollIntoView({block:'start',behavior:'auto'}); return true; } } return false;`,
);
await tape.sleep(500);

// Fire agent.send in the background — a session row should appear.
const session = (await tape.invoke("create_session", {
	workspaceId: bound.workspaceId,
})) as { sessionId: string };
const sendDriver = `
	window.__taper = window.__taper || {};
	var slot = (window.__taper.demoSend = { evs: [], done: false });
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
		workingDirectory: localDir,
		effortLevel: "medium",
		permissionMode: "bypassPermissions",
		fastMode: false,
	})};
	var p = window.__TAURI_INTERNALS__.invoke("send_agent_message_stream", { request: req, onEvent: ch });
	p["then"](function(){}, function(e){ slot.err = String(e && e.message ? e.message : e); slot.done = true; });
	return "started";
`;
await tape.js(sendDriver);

// Wait for the row to appear.
await tape.waitFor("[data-testid^=remote-agent-session-]", 30_000);
// Refresh sessions list so the row is fresh.
await tape.click("[aria-label^='Refresh agent sessions']").catch(() => {});
await tape.sleep(500);
await tape.scene({
	caption: "agent.send → daemon spawns the sidecar inside the container → live session row",
	record: 3,
	hold: 6,
});

// ── Beat 9 — everything green ──────────────────────────────────────
await tape.closeDialog();
await tape.sleep(500);
await tape.scene({
	caption: "All ops route to the container. Your laptop is just the viewport.",
	hold: 3,
});

// ── Beat 10 — docker stop, banner appears ──────────────────────────
const docker = async (...args: string[]) => {
	const p = Bun.spawn(["docker", ...args], { stdout: "pipe", stderr: "pipe" });
	const code = await p.exited;
	if (code !== 0) throw new Error(`docker ${args.join(" ")} → ${code}`);
};
tape.log(`stopping container ${CONTAINER}`);
await docker("stop", "-t", "1", CONTAINER);
// Wait for the liveness loop to flip the runtime state.
{
	const deadline = Date.now() + 20_000;
	while (Date.now() < deadline) {
		const rts = (await tape.invoke("list_remote_runtimes", {})) as Array<{
			name: string;
			state?: { type?: string };
		}>;
		const r = rts.find((x) => x.name === NAME);
		if (r && r.state?.type !== "connected") break;
		await tape.sleep(500);
	}
}
tape.assert(
	"banner_visible",
	await tape.waitFor(`[data-testid=remote-connection-banner-row-${NAME}]`, 10_000),
);
await tape.scene({
	caption: `docker stop → liveness ping fails → banner flips to Degraded`,
	hold: 6,
});

// ── Beat 11 — docker start + reconnect ─────────────────────────────
tape.log(`starting container ${CONTAINER}`);
await docker("start", CONTAINER);
await tape.sleep(3500);
const clicked = await tape.js<boolean>(
	`var r=document.querySelector('[data-testid=remote-connection-banner-row-${NAME}] button'); if(r){r.click(); return true;} return false;`,
);
if (!clicked) {
	await tape.invoke("reconnect_remote_runtime", { name: NAME }, 60_000);
}
// Wait for green.
{
	const deadline = Date.now() + 30_000;
	while (Date.now() < deadline) {
		const rts = (await tape.invoke("list_remote_runtimes", {})) as Array<{
			name: string;
			state?: { type?: string };
		}>;
		const r = rts.find((x) => x.name === NAME);
		if (r?.state?.type === "connected") break;
		await tape.sleep(500);
	}
}
await tape.scene({
	caption: "docker start → Reconnect → green. Same daemon, same workspace, same sessions.",
	record: 3,
	hold: 6,
});

// ── Beat 13 — close out ────────────────────────────────────────────
await tape.openSettings("remote-servers");
await tape.waitFor(`[data-testid=remote-server-row-${NAME}]`, 10_000);
await tape.scene({
	caption: "Everything Helmor wrote is in $HOME/.helmor/server/. Uninstall = rm -rf that one dir.",
	hold: 6,
});

const passed = await tape.finish({
	runtimeName: NAME,
	workspaceId: bound.workspaceId,
	remotePath: bound.remotePath,
	chipText,
});
process.exit(passed ? 0 : 1);
