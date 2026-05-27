#!/usr/bin/env bun
// scenarios/remote-runner.ts
//
// The flagship helmor-taper scenario: drive the live Helmor desktop to
// connect to a Dockerized Linux host running `helmor-server` over SSH,
// and show the remote runtime go green in the Remote Servers panel.
//
// Driven entirely through the MCP bridge (no OCR / synthetic input):
//   - open the panel via the shell event bus (`helmor:open-settings`)
//   - invoke the same backend command the Connect button calls
//     (`connect_remote_runtime`)
//   - read back the live `RuntimeHealth` for the bundle's assertions
//
// Runs on a DETERMINISTIC absolute timeline so the orchestrator can size
// the ScreenCaptureKit recording to match. Each "scene" lands at a fixed
// offset from start; the connect itself is ~1s so it fits comfortably.
//
// Env:
//   HOST_ALIAS     ssh-config alias for the docker host (default helmor-taper-arm64)
//   RUNTIME_NAME   label shown in the UI            (default docker-linux-arm64)
//   REMOTE_BINARY  daemon path on the remote        (default /home/e2e/.helmor/server/helmor-server)
//   ARTIFACT_DIR   where to write result.json       (default ./_artifacts)

import { Bridge } from "../scripts/mcp-bridge.ts";

const HOST_ALIAS = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const RUNTIME_NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const REMOTE_BINARY = process.env.REMOTE_BINARY ?? "/home/e2e/.helmor/server/helmor-server";
const ARTIFACT_DIR = process.env.ARTIFACT_DIR ?? "./_artifacts";
const ROW_TID = `remote-server-row-${RUNTIME_NAME}`;

const t0 = Date.now();
const log = (m: string) => console.error(`[+${String(Date.now() - t0).padStart(6)}ms] ${m}`);
/** Sleep until `ms` after scenario start (absolute timeline). */
const at = async (ms: number) => {
	const wait = ms - (Date.now() - t0);
	if (wait > 0) await Bun.sleep(wait);
};

async function openRemoteServersPanel(b: Bridge) {
	await b.executeJs(
		`window.dispatchEvent(new CustomEvent("helmor:open-settings",{detail:{section:"remote-servers"}})); return "ok";`,
	);
}
async function closeDialog(b: Bridge) {
	await b.executeJs(
		`document.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true})); return "esc";`,
	);
}
async function dom(b: Bridge): Promise<{ dialogs: number; empty: boolean; row: boolean; rowText: string | null }> {
	return b.executeJs(`
		var r = document.querySelector(${JSON.stringify(`[data-testid=${ROW_TID}]`)});
		return {
			dialogs: document.querySelectorAll("[role=dialog]").length,
			empty: !!document.querySelector("[data-testid=remote-servers-empty]"),
			row: !!r,
			rowText: r ? r.innerText.replace(/\\n+/g, " | ") : null
		};`) as Promise<{ dialogs: number; empty: boolean; row: boolean; rowText: string | null }>;
}

async function main() {
	const b = new Bridge();
	const port = await b.connect();
	log(`bridge connected on :${port}`);

	const assertions: Array<{ name: string; ok: boolean; detail: string }> = [];
	const assert = (name: string, ok: boolean, detail = "") => {
		assertions.push({ name, ok, detail });
		log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
	};

	// ── Precondition: clean slate (disconnected + no dialog) ───────────
	await b.invokeAndWait("disconnect_remote_runtime", { name: RUNTIME_NAME }, 30_000, "disc").catch(() => {});
	await closeDialog(b);
	await Bun.sleep(500);

	// ── Scene 1 (t=1.5s): empty Remote Servers panel ───────────────────
	await at(1_500);
	log("scene 1: open empty Remote Servers panel");
	await openRemoteServersPanel(b);
	await Bun.sleep(800);
	{
		const d = await dom(b);
		assert("panel_opens", d.dialogs >= 1, `dialogs=${d.dialogs}`);
		assert("starts_empty", d.empty, "no remote servers yet");
	}

	// ── Scene 2 (t=7s): fire the SSH connect ───────────────────────────
	await at(7_000);
	log(`scene 2: connect to ${HOST_ALIAS} over SSH`);
	await closeDialog(b);
	await Bun.sleep(400);
	const connectStart = Date.now();
	const health = (await b.invokeAndWait(
		"connect_remote_runtime",
		{ name: RUNTIME_NAME, host: HOST_ALIAS, remoteBinary: REMOTE_BINARY, forwardAgent: false },
		90_000,
		"conn",
	)) as { hostname?: string; version?: string; kind?: { type?: string; host?: string } };
	const connectMs = Date.now() - connectStart;
	assert("ssh_connect_succeeds", true, `${connectMs}ms`);
	assert(
		"daemon_reports_remote",
		health?.kind?.type === "remote" && health?.kind?.host === HOST_ALIAS,
		JSON.stringify(health?.kind ?? null),
	);
	assert("daemon_reports_version", typeof health?.version === "string" && /^\d+\.\d+\.\d+/.test(health.version), `v${health?.version}`);
	assert("daemon_reports_hostname", typeof health?.hostname === "string" && (health.hostname?.length ?? 0) > 0, health?.hostname);

	// ── Scene 3 (t=9s): reopen panel → connected row ───────────────────
	await at(9_000);
	log("scene 3: reopen panel → connected row");
	await openRemoteServersPanel(b);
	await Bun.sleep(1_500);
	{
		const d = await dom(b);
		assert("ui_shows_connected_row", d.row, d.rowText ?? "(no row)");
		assert("row_says_connected", !!d.rowText && /Connected/i.test(d.rowText), d.rowText ?? "");
	}

	// ── Confirm backend truth + hold the connected scene ───────────────
	const runtimes = (await b.invokeAndWait("list_remote_runtimes", {}, 15_000, "list")) as Array<{
		name: string;
		isLocal: boolean;
		state?: { type?: string };
		config?: unknown;
	}>;
	const remote = runtimes.find((r) => r.name === RUNTIME_NAME);
	assert("backend_runtime_connected", remote?.state?.type === "connected", JSON.stringify(remote?.state ?? null));

	// Hold the connected panel on screen for the viewer.
	await at(20_000);

	// ── Emit the result bundle ─────────────────────────────────────────
	const passed = assertions.every((a) => a.ok);
	const result = {
		scenario: "remote-runner",
		startedAt: new Date(t0).toISOString(),
		host: HOST_ALIAS,
		runtimeName: RUNTIME_NAME,
		remoteBinary: REMOTE_BINARY,
		connectMs,
		health,
		runtimes,
		assertions,
		passed,
	};
	await Bun.write(`${ARTIFACT_DIR}/result.json`, JSON.stringify(result, null, 2));
	log(`result.json written; passed=${passed}`);
	b.close();
	process.exit(passed ? 0 : 1);
}

await main();
