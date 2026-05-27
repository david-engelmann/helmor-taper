#!/usr/bin/env bun
// scenarios/connect-over-ssh.ts
//
// Proves: the Helmor desktop connects to a Dockerized Linux host running
// helmor-server over SSH, and the remote runtime goes green in the Remote
// Servers panel. The transport foundation of the whole feature.

import { type Assertion, Tape } from "./lib.ts";

const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const BIN = process.env.REMOTE_BINARY ?? "/home/e2e/.helmor/server/helmor-server";
const OUT = process.env.TAPE_DIR ?? "./tapes/connect-over-ssh";
const ROW = `[data-testid=remote-server-row-${NAME}]`;

const tape = new Tape("connect-over-ssh", OUT);
await tape.connect();

// Clean slate: disconnect + close any dialog.
await tape.invoke("disconnect_remote_runtime", { name: NAME }, 30_000).catch(() => {});
await tape.closeDialog();
await tape.sleep(500);

// Scene 1 — the empty panel.
await tape.openSettings("remote-servers");
await tape.sleep(900);
tape.assert("panel_opens", await tape.waitFor("[role=dialog]"));
tape.assert("starts_empty", await tape.waitFor("[data-testid=remote-servers-empty]", 3000));
await tape.scene({
	caption: "Settings → Remote Servers: no remote hosts yet",
	hold: 4,
});

// Scene 2 — fire the SSH connect (captures the connecting beat).
const start = Date.now();
void tape.bridge.invokeCommand(
	"connect_remote_runtime",
	{ name: NAME, host: HOST, remoteBinary: BIN, forwardAgent: false },
	"connect",
);
await tape.scene({
	caption: `Connecting to ${HOST} over SSH — Helmor installs + launches helmor-server`,
	record: 3,
	hold: 4,
});

// Wait for the connect to settle, capture health.
let health: { hostname?: string; version?: string; kind?: { type?: string; host?: string } } = {};
{
	const deadline = Date.now() + 60_000;
	while (Date.now() < deadline) {
		const r = await tape.bridge.pollResult("connect");
		if (r.done) {
			tape.assert("ssh_connect_succeeds", r.ok, r.ok ? `${Date.now() - start}ms` : String(r.error));
			health = (r.value as typeof health) ?? {};
			break;
		}
		await tape.sleep(400);
	}
}
tape.assert("daemon_reports_remote", health?.kind?.type === "remote" && health?.kind?.host === HOST, JSON.stringify(health?.kind ?? null));
tape.assert("daemon_reports_version", /^\d+\.\d+\.\d+/.test(health?.version ?? ""), `v${health?.version}`);

// Scene 3 — reopen the panel showing the connected row.
await tape.closeDialog();
await tape.sleep(300);
await tape.openSettings("remote-servers");
tape.assert("ui_shows_connected_row", await tape.waitFor(ROW, 8000));
const rowText = await tape.js<string | null>(
	`var r=document.querySelector(${JSON.stringify(ROW)}); return r?r.innerText.replace(/\\n+/g," · "):null;`,
);
tape.assert("row_says_connected", !!rowText && /Connected/i.test(rowText), rowText ?? "");
await tape.scene({
	caption: `Connected — helmor-server ${health?.version} live on ${health?.hostname}`,
	hold: 5,
});

const passed = await tape.finish({ host: HOST, runtimeName: NAME, health });
process.exit(passed ? 0 : 1);
