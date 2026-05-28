#!/usr/bin/env bun
// scenarios/observability.ts
//
// Proves Track E (observability): from the dev-only Runtime Debug panel an
// operator can, for a connected remote, read live SSH connection diagnostics
// (ping RTT + transport state), per-method RPC metrics (counts/errors/p50/p99),
// one-click a support-diagnostics bundle to the clipboard, and tail the remote
// daemon's log — all without leaving Helmor or shelling into the host.
//
// Each card lives in the same scrollable Settings pane, so the scenario scrolls
// the relevant section to the top before capturing its captioned beat.
//
// Preconditions: `bun run dev` (debug build → bridge + Runtime Debug panel) and
// a connected remote (scripts/setup-remote-workspace.ts). Discovers the remote
// from the registry so it survives renames.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/observability";

const tape = new Tape("observability", OUT);
await tape.connect();

// Confirm a remote is actually connected before we claim to observe it.
const runtimes = (await tape.invoke("list_remote_runtimes", {})) as Array<{
	name: string;
	state?: { type?: string };
}>;
const remote = runtimes.find((r) => r.name === NAME);
tape.assert("remote_connected", remote?.state?.type === "connected", `${NAME}=${remote?.state?.type}`);

/** Scroll the section containing `selector` to the top of the Settings pane. */
async function scrollToSection(selector: string): Promise<boolean> {
	return tape.js<boolean>(
		`var el=document.querySelector(${JSON.stringify(selector)}); if(!el) return false;` +
			`(el.closest('section')||el).scrollIntoView({block:'start',behavior:'auto'}); return true;`,
	);
}

// Clean shell, then open the Runtime Debug panel.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.openSettings("runtime-debug");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]"));

// ── Scene 1 — Connection diagnostics (live ping + state) ────────────
tape.assert("diagnostics_card", await tape.waitFor("[data-testid=connection-diagnostics-card]", 10_000));
tape.assert("ping_rtt_shown", await tape.waitFor("[data-testid=diagnostics-ping-ms]", 8000));
await scrollToSection("[data-testid=connection-diagnostics-card]");
await tape.sleep(800);
const diagText = await tape.js<string | null>(
	`var c=document.querySelector('[data-testid=connection-diagnostics-card]'); return c?c.innerText.replace(/\\n+/g," · ").slice(0,200):null;`,
);
tape.log(`diagnostics: ${diagText}`);
await tape.scene({
	caption: `Runtime Debug → Connection diagnostics: live SSH ping RTT, protocol handshake & transport state for ${NAME}`,
	hold: 5,
});

// ── Scene 2 — Per-method RPC metrics table ──────────────────────────
tape.assert("metrics_table", await tape.waitFor("[data-testid=runtime-metrics-table]", 10_000));
await scrollToSection("[data-testid=runtime-metrics-runtime-select]");
await tape.sleep(800);
const methodCount = await tape.js<number>(
	`var t=document.querySelector('[data-testid=runtime-metrics-table] tbody'); return t?t.querySelectorAll('tr').length:0;`,
);
tape.assert("metrics_have_rows", methodCount > 0, `${methodCount} methods`);
await tape.scene({
	caption: "Per-method RPC metrics: call counts, error counts, p50/p99 latency — read straight from the remote daemon",
	hold: 5,
});

// ── Scene 3 — Copy diagnostics bundle (one-click support blob) ──────
await tape.click("[data-testid=runtime-metrics-copy]");
const toast = await tape.waitFor("[data-sonner-toast]", 5000);
tape.assert("copy_toast", toast, toast ? "toast shown" : "no toast (clipboard may be denied in webview)");
await tape.scene({
	caption: "Copy diagnostics: one click bundles health + metrics + the last 50 daemon-log lines into a JSON blob for a support thread",
	record: 2,
	hold: 5,
});

// ── Scene 4 — Daemon log tail ───────────────────────────────────────
tape.assert("daemon_log_pre", await tape.waitFor("[data-testid=daemon-log-pre]", 10_000));
await scrollToSection("[data-testid=daemon-log-runtime-select]");
await tape.sleep(800);
const logLen = await tape.js<number>(
	`var p=document.querySelector('[data-testid=daemon-log-pre]'); return p?p.innerText.split("\\n").length:0;`,
);
tape.assert("daemon_log_has_lines", logLen > 0, `${logLen} lines`);
await tape.scene({
	caption: `Daemon log: tail $HOME/.helmor/server/daemon.log on ${NAME} without SSHing in — the first stop when an agent.send errors`,
	hold: 5,
});

const passed = await tape.finish({ runtimeName: NAME, methodCount, logLen });
process.exit(passed ? 0 : 1);
