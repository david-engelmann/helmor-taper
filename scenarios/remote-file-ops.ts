#!/usr/bin/env bun
// scenarios/remote-file-ops.ts
//
// Proves the core promise: when a workspace is bound to a remote runtime,
// every file-op (file tree, changes, file read, status) automatically runs
// on the CONTAINER, not the laptop. We use the Runtime Debug → Workspace
// inspector probe to round-trip `workspace.fileTree` + `workspace.changes`
// through the resolved runtime, with Auto-via-binding flipping the call
// onto docker-linux-arm64 by virtue of the workspace's `remote_path`.
//
// To make the proof unambiguous (the local and remote worktrees are the
// same repo, so READMEs look identical), we plant a REMOTE_ONLY_MARKER on
// the container before the second probe — its appearance as an untracked
// file conclusively shows the changes call hit the container, not the
// local worktree.

import { Tape } from "./lib.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";
const OUT = process.env.TAPE_DIR ?? "./tapes/remote-file-ops";

const tape = new Tape("remote-file-ops", OUT);
await tape.connect();

const bindings = (await tape.invoke("list_workspace_runtime_bindings", {})) as Array<{
	workspaceId: string;
	runtimeName: string;
	remotePath: string;
}>;
const bound = bindings.find((b) => b.runtimeName === NAME);
if (!bound) throw new Error(`no workspace bound to ${NAME}`);
tape.log(`bound: ${bound.workspaceId.slice(0, 8)} → ${bound.remotePath}`);

// The probe needs a LOCAL worktree path; the binding's remote_path is what
// `resolve_runtime_for_call` swaps in once it matches the workspace id.
const localDir = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/alnitak";

function setInputJs(idSel: string, value: string): string {
	return (
		`var el=document.querySelector(${JSON.stringify(idSel)});` +
		`if(!el) return "no-input";` +
		`var d=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value');` +
		`(d && d.set)?d.set.call(el,${JSON.stringify(value)}):(el.value=${JSON.stringify(value)});` +
		`el.dispatchEvent(new Event('input',{bubbles:true}));` +
		`return "ok";`
	);
}

async function clickButtonByText(text: string): Promise<boolean> {
	return tape.js<boolean>(
		`var bs=document.querySelectorAll('button');` +
			`for(var i=0;i<bs.length;i++){if((bs[i].innerText||'').trim()===${JSON.stringify(text)}){bs[i].click();return true;}}` +
			`return false;`,
	);
}

async function waitForText(scopeSel: string, needle: string, timeoutMs = 10_000): Promise<boolean> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		const hit = await tape.js<boolean>(
			`var s=document.querySelector(${JSON.stringify(scopeSel)});` +
				`return !!s && (s.innerText||'').indexOf(${JSON.stringify(needle)})>=0;`,
		);
		if (hit) return true;
		await tape.sleep(300);
	}
	return false;
}

// Open Runtime Debug + scroll the inspector probe to top.
await tape.js('window.location.reload(); return "r";');
await tape.sleep(6000);
await tape.openSettings("runtime-debug");
tape.assert("panel_opens", await tape.waitFor("[role=dialog]"));
await tape.sleep(400);
const scrolled = await tape.js<boolean>(
	`var el=document.querySelector('#inspector-probe-workspace');` +
		`if(!el) return false; (el.closest('section')||el).scrollIntoView({block:'start',behavior:'auto'}); return true;`,
);
tape.assert("probe_section_scrolled", scrolled);
await tape.sleep(400);

// Fill the form: leave runtime=Auto (via binding), provide workspaceId + dir.
await tape.js(setInputJs("#inspector-probe-workspace-id", bound.workspaceId));
await tape.js(setInputJs("#inspector-probe-workspace", localDir));
await tape.sleep(300);

// ── Scene 1: form filled, about to run ──────────────────────────────
await tape.scene({
	caption: `Workspace inspector probe → workspace ID + local worktree path. Runtime = "Auto (via binding)" → calls route via remote_path on docker-linux-arm64`,
	hold: 5,
});

// ── Scene 2: Run file tree → entries from the container ─────────────
const clickedTree = await clickButtonByText("Run file tree");
tape.assert("file_tree_clicked", clickedTree);
const treeRendered = await waitForText("[role=dialog]", "files (showing first", 15_000);
tape.assert("file_tree_rendered", treeRendered);
const treePreview = await tape.js<string | null>(
	`var lis=document.querySelectorAll('[role=dialog] li');` +
		`var paths=[]; for(var i=0;i<lis.length && i<6;i++){paths.push(lis[i].innerText.trim());}` +
		`return paths.join(' · ');`,
);
tape.log(`file tree preview: ${treePreview}`);
await tape.scene({
	caption: `Run file tree → entries returned from the container worktree at ${bound.remotePath} — same call shape, remote answer`,
	hold: 6,
});

// ── Scene 3: plant a remote-only marker + Run changes proves remote ──
const MARKER = "REMOTE_ONLY_MARKER.txt";
const MARKER_TEXT = `remote-proof-${Date.now()}`;
const planted = Bun.spawn([
	"docker", "exec", CONTAINER, "sh", "-c",
	`printf '%s' '${MARKER_TEXT}' > '${bound.remotePath}/${MARKER}'`,
]);
await planted.exited;
tape.log(`planted ${MARKER} on container`);

const clickedChanges = await clickButtonByText("Run changes");
tape.assert("changes_clicked", clickedChanges);
const changesRendered = await waitForText("[role=dialog]", "changed path", 15_000);
tape.assert("changes_rendered", changesRendered);
const sawMarker = await tape.js<boolean>(
	`return ((document.querySelector('[role=dialog]')||{}).innerText||'').indexOf(${JSON.stringify(MARKER)})>=0;`,
);
tape.assert("marker_in_changes", sawMarker, sawMarker ? "marker visible in changes list" : "marker missing");
await tape.scene({
	caption: `Planted ${MARKER} on the container → Run changes lists it as untracked. Proof: the call hit the container, not the local worktree.`,
	hold: 6,
});

const passed = await tape.finish({
	workspaceId: bound.workspaceId,
	remotePath: bound.remotePath,
	marker: MARKER,
	treePreview,
});
process.exit(passed ? 0 : 1);
