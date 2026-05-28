#!/usr/bin/env bun
// scenarios/add-remote-wizard.ts
//
// Proves Track B (Setup UX): the "Add remote server" wizard surfaces every
// SSH affordance an operator expects before committing to a connect — live
// agent state, identity list, ~/.ssh/config autocomplete + matched-host
// detail preview, and the agent-forward toggle — and exposes them BEFORE
// the network call. Mirrors VS Code's "Connect to host" + Zed's remote
// project flows without re-inventing credential capture (it deliberately
// reads ~/.ssh/config rather than asking the user to retype it).
//
// We type a real ssh-config alias (helmor-taper-arm64), show the detail
// preview surfacing hostname/port/identity straight from the user's
// config, and Cancel before connecting so the scenario is non-destructive
// w.r.t. the existing registered runtime.

import { Tape } from "./lib.ts";

const HOST_ALIAS = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/add-remote-wizard";

const tape = new Tape("add-remote-wizard", OUT);
await tape.connect();

// React-friendly input writer: hits the prototype setter then fires an
// `input` event the controlled component's onChange listens for.
function setInputJs(selector: string, value: string): string {
	return (
		`var el=document.querySelector(${JSON.stringify(selector)});` +
		`if(!el) return "no-input";` +
		`var d=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value');` +
		`(d && d.set)?d.set.call(el,${JSON.stringify(value)}):(el.value=${JSON.stringify(value)});` +
		`el.dispatchEvent(new Event('input',{bubbles:true}));` +
		`return "ok";`
	);
}

await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);

// Open Settings → Remote Servers, then open the wizard.
await tape.openSettings("remote-servers");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]"));
await tape.click("[data-testid=open-add-remote-server-wizard]");
tape.assert(
	"wizard_opens",
	await tape.waitFor("[data-testid=add-remote-server-wizard]", 5000),
);
tape.assert(
	"ssh_diagnostics_present",
	await tape.waitFor("[data-testid=ssh-diagnostics]", 3000),
);
const idRows = await tape.js<number>(
	`return document.querySelectorAll('[data-testid=ssh-identities-row]').length;`,
);
tape.assert("identities_listed", idRows > 0, `${idRows} rows`);

// ── Scene 1: empty wizard with diagnostics visible ──────────────────
await tape.scene({
	caption: "Settings → Remote Servers → Add remote server: SSH agent status + identities surface before any network call",
	hold: 5,
});

// ── Scene 2: type the host → ssh-config detail preview appears ──────
await tape.js(setInputJs("[data-testid=add-remote-server-name]", "demo-arm64"));
await tape.sleep(300);
await tape.js(setInputJs("[data-testid=add-remote-server-host]", HOST_ALIAS));
tape.assert(
	"host_detail_preview",
	await tape.waitFor("[data-testid=add-remote-server-host-detail]", 4000),
);
const detail = await tape.js<string | null>(
	`var d=document.querySelector('[data-testid=add-remote-server-host-detail]'); return d?d.innerText.replace(/\\n+/g," · ").slice(0,180):null;`,
);
tape.log(`host detail: ${detail}`);
await tape.scene({
	caption: `Typing "${HOST_ALIAS}" matches an ~/.ssh/config entry — Helmor previews hostname/port/identity straight from your config`,
	hold: 6,
});

// ── Scene 3: enable agent-forward, ready to connect ─────────────────
await tape.click("[data-testid=add-remote-server-forward-agent-input]");
const checked = await tape.js<boolean>(
	`return !!document.querySelector('[data-testid=add-remote-server-forward-agent-input]')?.checked;`,
);
tape.assert("forward_agent_toggled", checked);
await tape.scene({
	caption: "Forward SSH agent → the daemon will inherit your local keys for git fetch/push on private repos",
	hold: 5,
});

// Tidy: Cancel out — we don't want to register a duplicate runtime.
await tape.click("[data-testid=add-remote-server-cancel]");
await tape.sleep(400);

const passed = await tape.finish({ hostAlias: HOST_ALIAS, identityRows: idRows, hostDetail: detail });
process.exit(passed ? 0 : 1);
