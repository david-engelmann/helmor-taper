#!/usr/bin/env bun
// mcp-bridge.ts
//
// Minimal client for the `tauri-plugin-mcp-bridge` WebSocket protocol
// (the same wire format the MCP-server-for-Tauri speaks). Helmor's debug
// build hosts this bridge on 127.0.0.1:9223 (scanning up to +100 ports),
// gated behind `#[cfg(debug_assertions)]`. helmor-taper uses it to DRIVE
// the live Helmor UI deterministically while ScreenCaptureKit records the
// window — far more reliable than blind CGEventPost/OCR because we target
// real DOM elements and can invoke backend Tauri commands directly.
//
// Wire protocol (see tauri-plugin-mcp-bridge `websocket.rs`):
//   request : { "id": "<uuid>", "command": "<name>", "args": {...} }
//   response: { "id": "<uuid>", "success": true,  "data": ... }
//           | { "id": "<uuid>", "success": false, "error": "..." }
//
// Commands used here:
//   list_windows                 -> [{label,title,...}]
//   execute_js {windowLabel,script} -> { success, result }   (drives UI / invokes Tauri cmds)
//   capture_native_screenshot {windowLabel,format,quality,maxWidth} -> { dataUrl }
//   invoke_tauri {command:"plugin:mcp-bridge|start_ipc_monitor"|...} (IPC capture)
//
// Usage as a library:  import { Bridge } from "./mcp-bridge.ts"
// Usage as a CLI    :  bun mcp-bridge.ts <subcommand> [...]
//   ping                      connect + list windows
//   eval '<js>'               run JS in the Helmor window, print JSON result
//   invoke <cmd> '<json>'     invoke a backend Tauri command via execute_js
//   shot <out.png>            capture a native screenshot of the window
//   windows                   dump list_windows JSON

const DEFAULT_BASE_PORT = 9223;
const PORT_SCAN = 100;
const DEFAULT_WINDOW = "main";

type BridgeResponse = {
	id: string;
	success: boolean;
	data?: unknown;
	error?: string;
};

export class Bridge {
	private ws: WebSocket | null = null;
	private readonly pending = new Map<
		string,
		{ resolve: (v: BridgeResponse) => void; reject: (e: Error) => void }
	>();
	private port = 0;

	constructor(
		private readonly host = "127.0.0.1",
		private readonly basePort = DEFAULT_BASE_PORT,
	) {}

	/** Scan basePort..+PORT_SCAN for a live bridge and connect. */
	async connect(timeoutMs = 8000): Promise<number> {
		const deadline = Date.now() + timeoutMs;
		let lastErr: unknown;
		while (Date.now() < deadline) {
			for (let p = this.basePort; p < this.basePort + PORT_SCAN; p++) {
				try {
					await this.tryConnect(p);
					this.port = p;
					return p;
				} catch (e) {
					lastErr = e;
				}
			}
			await Bun.sleep(250);
		}
		throw new Error(
			`no MCP bridge on ${this.host}:${this.basePort}..+${PORT_SCAN} within ${timeoutMs}ms (last: ${lastErr})`,
		);
	}

	private tryConnect(port: number): Promise<void> {
		return new Promise((resolve, reject) => {
			const ws = new WebSocket(`ws://${this.host}:${port}`);
			const t = setTimeout(() => {
				ws.close();
				reject(new Error(`connect timeout :${port}`));
			}, 600);
			ws.addEventListener("open", () => {
				clearTimeout(t);
				this.ws = ws;
				ws.addEventListener("message", (ev) => this.onMessage(ev));
				ws.addEventListener("close", () => this.onClose());
				resolve();
			});
			ws.addEventListener("error", () => {
				clearTimeout(t);
				reject(new Error(`ws error :${port}`));
			});
		});
	}

	private onMessage(ev: MessageEvent) {
		let msg: BridgeResponse;
		try {
			msg = JSON.parse(String(ev.data));
		} catch {
			return; // broadcast/event frames without our id — ignore
		}
		if (!msg.id) return;
		const waiter = this.pending.get(msg.id);
		if (!waiter) return; // an IPC-monitor broadcast, not our reply
		this.pending.delete(msg.id);
		waiter.resolve(msg);
	}

	private onClose() {
		for (const { reject } of this.pending.values()) {
			reject(new Error("bridge connection closed"));
		}
		this.pending.clear();
		this.ws = null;
	}

	private send(command: string, args?: unknown): Promise<BridgeResponse> {
		if (!this.ws) throw new Error("not connected");
		const id = crypto.randomUUID();
		const payload = JSON.stringify({ id, command, args });
		return new Promise((resolve, reject) => {
			const t = setTimeout(() => {
				this.pending.delete(id);
				reject(new Error(`bridge command '${command}' timed out`));
			}, 60_000);
			this.pending.set(id, {
				resolve: (v) => {
					clearTimeout(t);
					resolve(v);
				},
				reject,
			});
			this.ws!.send(payload);
		});
	}

	private async ok(command: string, args?: unknown): Promise<unknown> {
		const r = await this.send(command, args);
		if (!r.success) throw new Error(`${command} failed: ${r.error}`);
		return r.data;
	}

	listWindows(): Promise<unknown> {
		return this.ok("list_windows");
	}

	/** Run JS in the webview and return its value. The bridge wraps the
	 *  script in `(function(){ <script> })()`, so the script must use
	 *  `return` to yield a value (a bare expression yields `undefined`).
	 *
	 *  IMPORTANT: the bridge's fast native (WKWebView) path is only taken
	 *  for SYNC scripts — any of `await `, `async `, `(async`, `.then(`,
	 *  `Promise.`, `new Promise(` forces a slower fallback that times out
	 *  here. Keep evaluated scripts synchronous; for async work use
	 *  `invokeCommand` (fire) + `pollResult` (poll). */
	executeJs(script: string, windowLabel = DEFAULT_WINDOW): Promise<unknown> {
		return this.ok("execute_js", { windowLabel, script });
	}

	/** Fire a backend Tauri command from inside the webview WITHOUT awaiting
	 *  it (the evaluated script stays synchronous so it takes the native
	 *  path). The promise resolves in the background and stashes its outcome
	 *  on `window.__taper[slot]`; poll it with `pollResult(slot)`.
	 *
	 *  `p["then"](...)` is used deliberately instead of `p.then(...)` so the
	 *  script text doesn't trip the bridge's async-detection substring scan. */
	async invokeCommand(
		cmd: string,
		args: Record<string, unknown> = {},
		slot = "last",
	): Promise<void> {
		const script = `
			window.__taper = window.__taper || {};
			var s = (window.__taper[${JSON.stringify(slot)}] = { done:false, ok:false, value:null, error:null });
			var invoke = (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke)
				|| (window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke);
			if (!invoke) { s.done = true; s.error = "no Tauri invoke on window"; return "no-invoke"; }
			var p = invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)});
			p["then"](function(v){ s.ok = true; s.value = v; s.done = true; },
			          function(e){ s.error = String((e && e.message) ? e.message : e); s.done = true; });
			return "started";`;
		await this.executeJs(script);
	}

	/** Read the stashed outcome of a prior `invokeCommand(cmd, args, slot)`. */
	pollResult(slot = "last"): Promise<{ done: boolean; ok: boolean; value: unknown; error: string | null }> {
		const script = `
			var s = (window.__taper && window.__taper[${JSON.stringify(slot)}]) || { done:false, ok:false, value:null, error:null };
			return { done: !!s.done, ok: !!s.ok, value: s.value, error: s.error };`;
		return this.executeJs(script) as Promise<{ done: boolean; ok: boolean; value: unknown; error: string | null }>;
	}

	/** Fire a command and poll until it settles (or times out). Returns the
	 *  resolved value, or throws with the command's rejection message. */
	async invokeAndWait(
		cmd: string,
		args: Record<string, unknown> = {},
		timeoutMs = 90_000,
		slot = "last",
	): Promise<unknown> {
		await this.invokeCommand(cmd, args, slot);
		const deadline = Date.now() + timeoutMs;
		while (Date.now() < deadline) {
			const r = await this.pollResult(slot);
			if (r.done) {
				if (!r.ok) throw new Error(`${cmd} rejected: ${r.error}`);
				return r.value;
			}
			await Bun.sleep(400);
		}
		throw new Error(`${cmd} did not settle within ${timeoutMs}ms`);
	}

	/** Native window screenshot -> writes PNG to `outPath`. */
	async screenshot(outPath: string, windowLabel = DEFAULT_WINDOW): Promise<void> {
		const r = await this.send("capture_native_screenshot", {
			windowLabel,
			format: "png",
			quality: 90,
		});
		const data = r as unknown as { dataUrl?: string; data?: string; success?: boolean; error?: string };
		const url = data.dataUrl ?? data.data;
		if (!url) throw new Error(`screenshot failed: ${data.error ?? JSON.stringify(r).slice(0, 200)}`);
		const b64 = url.includes(",") ? url.split(",")[1] : url;
		await Bun.write(outPath, Buffer.from(b64, "base64"));
	}

	startIpcMonitor(): Promise<BridgeResponse> {
		return this.send("invoke_tauri", { command: "plugin:mcp-bridge|start_ipc_monitor", args: {} });
	}
	stopIpcMonitor(): Promise<BridgeResponse> {
		return this.send("invoke_tauri", { command: "plugin:mcp-bridge|stop_ipc_monitor", args: {} });
	}
	getIpcEvents(): Promise<BridgeResponse> {
		return this.send("invoke_tauri", { command: "plugin:mcp-bridge|get_ipc_events", args: {} });
	}

	close() {
		this.ws?.close();
	}
}

// ── CLI ─────────────────────────────────────────────────────────────
if (import.meta.main) {
	const [sub, ...rest] = Bun.argv.slice(2);
	const b = new Bridge();
	const port = await b.connect();
	const out = (v: unknown) => console.log(JSON.stringify(v, null, 2));
	try {
		switch (sub) {
			case "ping":
			case "windows":
				out({ port, windows: await b.listWindows() });
				break;
			case "eval":
				out(await b.executeJs(rest[0] ?? "1+1"));
				break;
			case "invoke":
				out(await b.invokeAndWait(rest[0], rest[1] ? JSON.parse(rest[1]) : {}));
				break;
			case "shot":
				await b.screenshot(rest[0] ?? "/tmp/helmor-shot.png");
				console.error(`wrote ${rest[0] ?? "/tmp/helmor-shot.png"}`);
				break;
			default:
				console.error("usage: mcp-bridge.ts <ping|windows|eval|invoke|shot> [...]");
				process.exit(2);
		}
	} finally {
		b.close();
	}
}
