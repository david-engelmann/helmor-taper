#!/usr/bin/env bun
// scenarios/resilience.ts
//
// Proves Track C (resilience): when the remote host goes away — modeled here
// by `docker stop` of the container hosting helmor-server — Helmor's liveness
// loop notices, flips the runtime to Degraded → Disconnected, surfaces a
// banner across the top of the app, and offers a one-click Reconnect that
// re-establishes SSH + the JSON-RPC pipe once the host comes back.
//
// This is the headline "is this production-ready?" video: a real failure
// (container down), a real detection (liveness ping fails), a real recovery
// (Reconnect mutation succeeds, runtime goes green again).
//
// Preconditions: `bun run dev` (debug build → bridge), helmor-test-linux-arm64
// container running + a connected `docker-linux-arm64` runtime. The scenario
// stops the container, waits for state to flip, restarts it, and reconnects.

import { type Assertion, Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/resilience";

const tape = new Tape("resilience", OUT);
await tape.connect();

async function docker(...args: string[]): Promise<string> {
	const p = Bun.spawn(["docker", ...args], { stdout: "pipe", stderr: "pipe" });
	const code = await p.exited;
	const out = await new Response(p.stdout).text();
	if (code !== 0) throw new Error(`docker ${args.join(" ")} → ${code}: ${await new Response(p.stderr).text()}`);
	return out.trim();
}

async function waitForState(target: (s: string) => boolean, timeoutMs: number): Promise<string> {
	const deadline = Date.now() + timeoutMs;
	let last = "(unknown)";
	while (Date.now() < deadline) {
		const rts = (await tape.invoke("list_remote_runtimes", {})) as Array<{
			name: string;
			state?: { type?: string };
		}>;
		const r = rts.find((x) => x.name === NAME);
		last = r?.state?.type ?? "(missing)";
		if (target(last)) return last;
		await tape.sleep(500);
	}
	return last;
}

// 0. Make sure we start connected — if the user's been kicking the runtime,
//    reconnect first so the failure→recovery story is meaningful.
{
	const initialState = await waitForState((s) => s === "connected", 5_000);
	if (initialState !== "connected") {
		tape.log(`runtime ${NAME} state=${initialState}; reconnecting first`);
		await tape.invoke("reconnect_remote_runtime", { name: NAME }, 60_000);
		await waitForState((s) => s === "connected", 30_000);
	}
}

// Reload to clean shell, then open Remote Servers so the row is visible too.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.openSettings("remote-servers");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]"));
tape.assert("row_present", await tape.waitFor(`[data-testid=remote-server-row-${NAME}]`, 10_000));

// ── Scene 1: connected baseline ─────────────────────────────────────
await tape.scene({
	caption: `Baseline: ${NAME} is connected — helmor-server is alive on the container`,
	hold: 4,
});

// ── Scene 2: kill the host. Close the dialog so the banner is visible. ─
await tape.closeDialog();
await tape.sleep(400);
tape.log(`stopping container ${CONTAINER}`);
await docker("stop", "-t", "1", CONTAINER);

// Liveness pings every 5s with a 3s timeout. The state flip is usually
// visible within one cycle; wait up to 20s for it to settle on a non-
// connected state.
const downState = await waitForState((s) => s !== "connected", 20_000);
tape.assert("state_flips_offline", downState !== "connected", `state=${downState}`);
tape.assert(
	"banner_appears",
	await tape.waitFor(`[data-testid=remote-connection-banner-row-${NAME}]`, 10_000),
	"top-of-shell banner",
);

await tape.scene({
	caption: `docker stop ${CONTAINER} → liveness ping fails → banner flips to "${downState}"`,
	record: 3,
	hold: 6,
});

// ── Scene 3: bring it back + reconnect ──────────────────────────────
tape.log(`starting container ${CONTAINER}`);
await docker("start", CONTAINER);
// sshd needs a beat to be ready after start. Don't busy-poll the bridge —
// just give it a fixed grace period, then kick reconnect.
await tape.sleep(3500);

// Click Reconnect on the banner row if it offers one; fall back to the
// backend mutation so the scenario survives banner-styling tweaks.
const clicked = await tape.js<boolean>(
	`var r=document.querySelector('[data-testid=remote-connection-banner-row-${NAME}] button'); if(r){r.click(); return true;} return false;`,
);
if (clicked) {
	tape.log("clicked Reconnect button in banner");
} else {
	tape.log("no banner button; falling back to reconnect_remote_runtime");
	await tape.invoke("reconnect_remote_runtime", { name: NAME }, 60_000);
}

const recovered = await waitForState((s) => s === "connected", 60_000);
tape.assert("state_recovers", recovered === "connected", `state=${recovered}`);

// Banner should auto-disappear when state goes back to connected. Open the
// Remote Servers panel again so the row's "Connected" chip is on screen.
await tape.openSettings("remote-servers");
await tape.waitFor(`[data-testid=remote-server-row-${NAME}]`, 10_000);
await tape.sleep(800);

await tape.scene({
	caption: `Reconnect → SSH re-establishes → ${NAME} green again. No restart, no losing your work.`,
	hold: 6,
});

const passed = await tape.finish({ runtimeName: NAME, downState });
process.exit(passed ? 0 : 1);
