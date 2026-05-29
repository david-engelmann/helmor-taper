#!/usr/bin/env bun
// scenarios/first-connect-bundle.ts
//
// Headline demo for the productionized install flow: a fresh host
// (with helmor-server but no bundle) gets the agent runtime installed
// automatically on connect, and the Remote Servers row's live chip
// narrates what's happening end-to-end.
//
// Pre-record: the test container's `$HOME/.helmor/server/` has only
// `helmor-server` (the daemon binary). No sidecar, no claude, no
// wrapper script — the "I just SSH'd a stock Linux box" state.
//
// Captured beats (one scene per visual state):
//   1. The Remote Servers panel, freshly opened, showing the
//      connected runtime with its state chip but no agent-runtime
//      chip — the "before" baseline.
//   2. The chip turning blue with a spinner + the live phase
//      message ("Uploading agent runtime (3 files, 325.9 MB)") —
//      proof that the operator can see what's happening while it
//      happens, no opaque progress bar.
//   3. The chip green: "Agent runtime installed in 5.3s" — the
//      "done" beat that confirms a successful install.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/first-connect-bundle";

const tape = new Tape("first-connect-bundle", OUT);
await tape.connect();

// 0. Wipe the bundle so this records a true cold install. Restoring
//    the daemon binary's name (the `.real → helmor-server` move) is
//    what makes the install treat this as fresh-host.
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
if ((await wipe.exited) !== 0) {
	throw new Error("could not wipe container bundle artifacts");
}
tape.log("wiped container bundle artifacts");

// 1. Reload to a clean shell, then open Settings → Remote Servers.
//    Skip an explicit disconnect — `connect_remote_runtime` is
//    idempotent on the registry side and the install routine is
//    safe to re-trigger. We just need to land in the panel.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.openSettings("remote-servers");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]", 10_000));
tape.assert(
	"row_present",
	await tape.waitFor(`[data-testid=remote-server-row-${NAME}]`, 10_000),
);

await tape.scene({
	caption: `Remote Servers panel — ${NAME} is connected but has no agent runtime yet`,
	hold: 4,
});

// 2. Fire connect_remote_runtime in the background. We don't wait
//    for it to settle here — the whole point of this tape is to
//    capture the chip's progress, which only renders WHILE the
//    install is in flight. The chip's `installing` state lasts a
//    few seconds (the LAN install is ~5s), so the next scene
//    captures it mid-stream.
void tape.bridge.invokeCommand(
	"connect_remote_runtime",
	{ name: NAME, host: HOST, remoteBinary: "/home/e2e/.helmor/server/helmor-server", forwardAgent: false },
	"connect",
);

// Wait for the chip to flip into installing state.
const installingChip = await tape.waitFor(
	`[data-testid=remote-server-bundle-installing-${NAME}]`,
	10_000,
);
tape.assert("bundle_chip_installing", installingChip);

// Live-capture 3 s of the chip transitioning between phases (the
// recorder catches the actual frames; scene() runs ScreenCaptureKit
// in parallel with the UI animating).
await tape.scene({
	caption: `Auto-install: agent runtime streams to the container, sha-verified, atomic per file`,
	record: 4,
	hold: 6,
});

// 3. Wait for the install to settle into the success chip.
const installedChip = await tape.waitFor(
	`[data-testid=remote-server-bundle-installed-${NAME}]`,
	60_000,
);
tape.assert("bundle_chip_installed", installedChip);
const chipText = await tape.js<string | null>(
	`var c = document.querySelector('[data-testid=remote-server-bundle-installed-${NAME}]');` +
		`return c ? c.innerText : null;`,
);
tape.log(`chip says: ${chipText}`);

await tape.scene({
	caption: `Done — ${chipText ?? "agent runtime installed"}. Connect, install, ready in under 10 s.`,
	hold: 5,
});

const passed = await tape.finish({
	runtimeName: NAME,
	chipText,
});
process.exit(passed ? 0 : 1);
