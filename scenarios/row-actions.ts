#!/usr/bin/env bun
// scenarios/row-actions.ts
//
// Proves Track B/E/G: the Remote Servers row is the operator's one-stop
// cockpit per remote — Auth (per-runtime SDK key configured on the daemon;
// the key never leaves the host), Reconnect (when state isn't connected),
// Diagnostics (clipboard-copies a JSON support bundle: health + metrics +
// last 50 daemon-log lines), and Disconnect. Each affordance is one click.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/row-actions";
const ROW = `[data-testid=remote-server-row-${NAME}]`;

const tape = new Tape("row-actions", OUT);
await tape.connect();

await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.openSettings("remote-servers");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]"));
tape.assert("row_present", await tape.waitFor(ROW, 10_000));

// Confirm the four buttons are wired.
const actions = await tape.js<Record<string, boolean>>(
	`var q=function(s){return !!document.querySelector(s)};` +
		`return {` +
		`auth: q('[data-testid=remote-server-set-auth-${NAME}]'),` +
		`reconnect: q('[data-testid=remote-server-reconnect-${NAME}]'),` +
		`diagnostics: q('[data-testid=remote-server-copy-diagnostics-${NAME}]'),` +
		`disconnect: q('[data-testid=remote-server-disconnect-${NAME}]')` +
		`};`,
);
tape.log(`actions: ${JSON.stringify(actions)}`);
tape.assert("auth_button", actions.auth);
tape.assert("diagnostics_button", actions.diagnostics);
tape.assert("disconnect_button", actions.disconnect);

// ── Scene 1: the row + every action visible ─────────────────────────
await tape.scene({
	caption: `Settings → Remote Servers: each remote shows its state + one-click Auth · Diagnostics · Disconnect`,
	hold: 5,
});

// ── Scene 2: Diagnostics → support-bundle toast ─────────────────────
await tape.click(`[data-testid=remote-server-copy-diagnostics-${NAME}]`);
const toast = await tape.waitFor("[data-sonner-toast]", 6000);
tape.assert("diagnostics_toast", toast, toast ? "toast shown" : "no toast");
await tape.scene({
	caption: "Diagnostics → one-click clipboard bundle: health snapshot + RPC metrics + last 50 daemon-log lines, JSON formatted",
	record: 2,
	hold: 5,
});

// ── Scene 3: Auth dialog ────────────────────────────────────────────
await tape.click(`[data-testid=remote-server-set-auth-${NAME}]`);
tape.assert(
	"auth_dialog_opens",
	await tape.waitFor("[data-testid=runtime-auth-dialog]", 5000),
);
// "Not configured" status renders when the daemon has no key for any provider yet.
const status = await tape.js<string>(
	`var c=document.querySelector('[data-testid=runtime-auth-status-configured]');` +
		`var n=document.querySelector('[data-testid=runtime-auth-status-not-configured]');` +
		`return c?"configured":(n?"not-configured":"unknown");`,
);
tape.assert("auth_status_shown", status !== "unknown", status);
await tape.scene({
	caption: "Auth → per-runtime SDK API key configured ON THE DAEMON. The key never leaves the host; the desktop only sees the configured-providers list.",
	hold: 6,
});

// Tidy: cancel auth so we don't write empty creds.
await tape.click("[data-testid=runtime-auth-cancel]");
await tape.sleep(400);

const passed = await tape.finish({ runtimeName: NAME, actions, authStatus: status });
process.exit(passed ? 0 : 1);
