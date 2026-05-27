#!/usr/bin/env bun
// scenarios/remote-workspace.ts
//
// Proves: a workspace bound to the remote is unmistakably marked as such.
// Selecting it surfaces the blue runtime chip in the header AND the sidebar
// row — the always-on "you are working on docker-linux-arm64, not your
// laptop" cue (Helmor's per-workspace analog of VS Code's remote indicator).
//
// Assumes a remote-bound workspace already exists (run
// scripts/setup-remote-workspace.ts first). Finds it by its binding so it
// survives the ephemeral workspace ids dev regenerates.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/remote-workspace";

const tape = new Tape("remote-workspace", OUT);
await tape.connect();

// Find the workspace bound to the remote.
const bindings = (await tape.invoke("list_workspace_runtime_bindings", {})) as Array<{
	workspaceId: string;
	runtimeName: string;
}>;
const bound = bindings.find((b) => b.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}; run setup-remote-workspace.ts first`);
const WS = bound.workspaceId;
tape.log(`bound workspace: ${WS}`);

// Reload to a clean shell, then select the bound workspace.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
tape.assert("row_present", await tape.waitFor(`[data-workspace-row-id="${WS}"]`, 10_000));
await tape.js(
	`var el=document.querySelector('[data-workspace-row-id="${WS}"] [data-workspace-row-body]')||document.querySelector('[data-workspace-row-id="${WS}"]'); if(el) el.click(); return "clicked";`,
);
await tape.sleep(1500);

// Confirm the chip is live in the header.
const chip = await tape.js<{ present: boolean; label: string | null }>(
	`var c=document.querySelector('[aria-label^="Workspace runtime:"]'); return { present: !!c, label: c?c.getAttribute("aria-label"):null };`,
);
tape.assert("header_chip_visible", chip.present, chip.label ?? "(none)");
tape.assert("chip_names_runtime", (chip.label ?? "").includes(NAME), chip.label ?? "");

// Scene 1 — the bound workspace with the chip in header + sidebar.
await tape.scene({
	caption: `This workspace runs on ${NAME} — the blue chip marks it in the header & sidebar`,
	hold: 5,
});

const passed = await tape.finish({ runtimeName: NAME, workspaceId: WS, chip });
process.exit(passed ? 0 : 1);
