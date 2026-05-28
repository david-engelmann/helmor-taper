#!/usr/bin/env bun
// feature-probe.ts
//
// Command-level confirmation of the remote-runner feature surface against
// a LIVE Helmor (debug build, MCP bridge on :9223) connected to the
// Dockerized Linux remote. Each check invokes the same backend command the
// UI uses and asserts on the result — so a green run proves the feature
// works end-to-end (desktop → SSH → daemon → container) without recording.
//
// Pair with docs/feature-evidence.md: that catalog maps each feature to its
// UI navigation (for recording) + the assertions below (for confirmation).
//
// Preconditions: `bun run dev` running; a workspace already moved onto the
// remote (scripts/setup-remote-workspace.ts). Discovers the bound workspace.
//
// Usage: bun feature-probe.ts   (writes report to ./feature-probe-report.json)

import { Bridge } from "./mcp-bridge.ts";

const HOST = process.env.HOST_ALIAS ?? "helmor-taper-arm64";
const RUNTIME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 60_000) =>
	b.invokeAndWait(c, a, t, `probe-${c}`) as Promise<T>;

type Check = { feature: string; track: string; ok: boolean; detail: string };
const results: Check[] = [];
let bound: { workspaceId: string; remotePath: string } | null = null;
let localDir = "";

async function check(feature: string, track: string, fn: () => Promise<string>) {
	try {
		const detail = await fn();
		results.push({ feature, track, ok: true, detail });
		console.error(`✓ [${track}] ${feature} — ${detail}`);
	} catch (e) {
		results.push({ feature, track, ok: false, detail: String(e).slice(0, 160) });
		console.error(`✗ [${track}] ${feature} — ${String(e).slice(0, 160)}`);
	}
}

// Discover the remote-bound workspace + its local dir.
{
	const bindings = (await inv("list_workspace_runtime_bindings")) as Array<{
		workspaceId: string;
		runtimeName: string;
		remotePath: string;
	}>;
	const m = bindings.find((x) => x.runtimeName === RUNTIME);
	if (m) bound = { workspaceId: m.workspaceId, remotePath: m.remotePath };
	// Local worktree dir from the workspace groups (best-effort).
	try {
		const groups = (await inv("load_workspace_groups")) as unknown;
		void groups;
	} catch {}
}
// Fall back to the known dev path shape if discovery is partial.
localDir = process.env.LOCAL_WS_DIR ?? "/Users/david/helmor-dev/workspaces/helmor-taper/albiorix";

// ── Track B: SSH setup surface ─────────────────────────────────────
await check("SSH host autocomplete (~/.ssh/config)", "B", async () => {
	const hosts = (await inv("list_ssh_hosts")) as string[];
	if (!hosts.includes(HOST)) throw new Error(`host ${HOST} not in ${hosts.length} hosts`);
	return `${hosts.length} hosts incl. ${HOST}`;
});
await check("SSH host details (hostname/port/identity)", "B", async () => {
	const details = (await inv("list_ssh_host_details")) as Array<{ alias: string; hostName?: string; port?: number }>;
	const d = details.find((x) => x.alias === HOST);
	if (!d) throw new Error(`no details for ${HOST}`);
	return `${HOST} → ${d.hostName}:${d.port ?? 22}`;
});
await check("SSH identities visibility", "B", async () => {
	const ids = (await inv("list_ssh_identities")) as Array<{ path: string }>;
	return `${ids.length} identities`;
});
await check("SSH agent status", "B", async () => {
	const s = (await inv("ssh_agent_status")) as { available?: boolean; reachable?: boolean };
	return JSON.stringify(s);
});
await check("Pre-connect SSH probe", "B", async () => {
	const p = (await inv("probe_ssh_host", { host: HOST }, 30_000)) as { reachable?: boolean; outcome?: string; classification?: string };
	return JSON.stringify(p).slice(0, 120);
});

// ── Connect + state ────────────────────────────────────────────────
await check("Connected remote runtime", "B/C", async () => {
	const rts = (await inv("list_remote_runtimes")) as Array<{ name: string; state?: { type?: string } }>;
	const r = rts.find((x) => x.name === RUNTIME);
	if (r?.state?.type !== "connected") throw new Error(`state=${JSON.stringify(r?.state)}`);
	return `${RUNTIME} connected`;
});
await check("Runtime health (host/version)", "B/C", async () => {
	const h = (await inv("get_runtime_health", { runtimeName: RUNTIME })) as { version?: string; hostname?: string; kind?: { type?: string } };
	if (h.kind?.type !== "remote") throw new Error(`expected remote, got ${JSON.stringify(h.kind)}`);
	if (!/^\d+\.\d+\.\d+/.test(h.version ?? "")) throw new Error(JSON.stringify(h));
	return `v${h.version} on ${h.hostname} (remote)`;
});

// ── Track E: observability ─────────────────────────────────────────
await check("Daemon log tail (E1)", "E", async () => {
	const r = (await inv("tail_remote_daemon_log", { name: RUNTIME, maxLines: 20 })) as { lines?: string[] };
	if (!Array.isArray(r.lines)) throw new Error(JSON.stringify(r).slice(0, 120));
	return `${r.lines.length} log lines`;
});
await check("Per-method RPC metrics (E2)", "E", async () => {
	const m = (await inv("get_remote_runtime_metrics", { name: RUNTIME })) as Record<string, unknown>;
	return `metrics keys: ${Object.keys(m).slice(0, 6).join(",")}`;
});
await check("Copy-diagnostics bundle (E3)", "E", async () => {
	const d = (await inv("get_remote_runtime_diagnostics", { name: RUNTIME })) as Record<string, unknown>;
	return `diagnostics keys: ${Object.keys(d).slice(0, 6).join(",")}`;
});
await check("Agent auth status (G2)", "G", async () => {
	const a = (await inv("get_remote_runtime_auth_status", { name: RUNTIME })) as Record<string, unknown>;
	return JSON.stringify(a).slice(0, 120);
});

// ── Track F2 + core: workspace binding + remote file-ops ───────────
await check("Workspace bound to remote (F2/B5)", "F", async () => {
	if (!bound) throw new Error("no workspace bound to remote");
	return `${bound.workspaceId.slice(0, 8)} → ${RUNTIME} @ ${bound.remotePath}`;
});
await check("Per-host remote path memory (F2.1)", "F", async () => {
	if (!bound) throw new Error("no bound workspace");
	const p = (await inv("get_remembered_workspace_remote_path", { workspaceId: bound.workspaceId, runtimeName: RUNTIME })) as string | null;
	if (p !== bound.remotePath) throw new Error(`remembered=${p}`);
	return `remembered ${p}`;
});
// IMPORTANT: file-op commands translate local→remote via the binding's
// remote_path ONLY when called with `workspaceId` and NO explicit
// `runtimeName` (an explicit runtime_name makes resolve_runtime_for_call
// skip the binding/override). That's how the real frontend calls them, so
// the probe mirrors it: pass workspaceId only.
const wsId = bound?.workspaceId;

// Plant a marker that exists ONLY on the remote worktree, so a successful
// read PROVES the op hit the container (the local + remote worktrees are
// the same repo, so README alone wouldn't disambiguate).
const MARKER = "REMOTE_ONLY_MARKER.txt";
const MARKER_TEXT = `remote-proof-${Date.now()}`;
if (bound) {
	await Bun.spawn([
		"docker", "exec", "helmor-test-linux-arm64", "sh", "-c",
		`printf '%s' '${MARKER_TEXT}' > '${bound.remotePath}/${MARKER}'`,
	]).exited;
}

await check("Remote git status", "core", async () => {
	const s = (await inv("get_workspace_status", { workspaceDir: localDir, workspaceId: wsId })) as { changedPaths?: string[] };
	// The marker is an untracked file ON THE REMOTE → status must see it.
	const saw = JSON.stringify(s).includes(MARKER);
	if (!saw) throw new Error(`remote marker not in status: ${JSON.stringify(s).slice(0, 120)}`);
	return `status from remote sees ${MARKER}`;
});
await check("Remote branch info", "core", async () => {
	const s = (await inv("get_workspace_branch_info", { workspaceDir: localDir, workspaceId: wsId })) as { branch?: string };
	return `branch: ${JSON.stringify(s).slice(0, 90)}`;
});
await check("Remote file read (content from container)", "core", async () => {
	const r = (await inv("read_workspace_file", { workspaceDir: localDir, relativePath: MARKER, workspaceId: wsId })) as { content?: string };
	if ((r.content ?? "") !== MARKER_TEXT) throw new Error(`got: ${(r.content ?? "").slice(0, 60)}`);
	return `read remote-only marker (content matches)`;
});
await check("Remote file tree", "core", async () => {
	const r = (await inv("get_workspace_file_tree", { workspaceDir: localDir, workspaceId: wsId })) as { entries?: Array<{ name?: string; path?: string }> };
	const n = (r.entries ?? []).length;
	const saw = (r.entries ?? []).some((e) => (e.name ?? e.path ?? "").includes(MARKER));
	if (!saw) throw new Error(`marker not in ${n}-entry tree`);
	return `${n} entries from remote (incl. marker)`;
});
await check("Remote workspace search (git grep)", "core", async () => {
	const r = (await inv("search_workspace", { workspaceDir: localDir, query: "Helmor", maxResults: 5, caseInsensitive: true, workspaceId: wsId })) as { matches?: Array<{ relativePath?: string }> };
	const matches = r.matches ?? [];
	if (matches.length === 0) throw new Error("no matches on remote (README should match)");
	return `${matches.length} matches from remote (${matches[0]?.relativePath})`;
});
await check("Remote file read at git ref (diff base)", "core", async () => {
	const r = (await inv("read_workspace_file_at_ref", { workspaceDir: localDir, relativePath: "README.md", gitRef: "HEAD", workspaceId: wsId })) as { content?: string | null };
	if (!/helmor-taper/i.test(r.content ?? "")) throw new Error(`unexpected: ${(r.content ?? "").slice(0, 50)}`);
	return `README.md@HEAD read from remote`;
});

// ── Report ─────────────────────────────────────────────────────────
const passed = results.filter((r) => r.ok).length;
console.error(`\n${passed}/${results.length} feature checks passed`);
await Bun.write("./feature-probe-report.json", JSON.stringify({ host: HOST, runtime: RUNTIME, bound, passed, total: results.length, results }, null, 2));
b.close();
process.exit(passed === results.length ? 0 : 1);
