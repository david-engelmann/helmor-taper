// scenarios/lib.ts
//
// Shared helpers for helmor-taper scenarios. A scenario drives the live
// Helmor UI through the MCP bridge and, between drive steps, captures
// captioned scene clips of the window. At the end it concatenates the
// scenes into master.mp4 + master.gif and writes result.json.
//
// The scene-clip model (see scripts/capture-scene.ts) is deliberate:
// ScreenCaptureKit compresses static holds, so one long timed-caption
// recording drifts — capturing each beat as its own freeze-padded clip
// keeps every caption perfectly aligned to what's on screen.

import { Bridge } from "../scripts/mcp-bridge.ts";

const ROOT = new URL("..", import.meta.url).pathname;
export const PROC = process.env.PROC_NAME ?? "Helmor";
export const SCALE_W = process.env.SCALE_W ?? "1100";

export type SceneSpec = {
	/** caption burned across the top of this beat */
	caption: string;
	/** live ScreenCaptureKit capture seconds (catches motion) */
	record?: number;
	/** total clip seconds; last frame freezes to fill */
	hold?: number;
};

export type Assertion = { name: string; ok: boolean; detail?: string };

export class Tape {
	readonly bridge = new Bridge();
	private readonly scenes: string[] = [];
	private sceneIdx = 0;
	readonly assertions: Assertion[] = [];
	readonly t0 = Date.now();

	constructor(
		readonly name: string,
		readonly outDir: string,
	) {}

	log(m: string) {
		console.error(`[${this.name} +${((Date.now() - this.t0) / 1000).toFixed(1)}s] ${m}`);
	}

	async connect(): Promise<void> {
		const port = await this.bridge.connect();
		this.log(`bridge :${port}`);
	}

	assert(name: string, ok: boolean, detail = "") {
		this.assertions.push({ name, ok, detail });
		this.log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` — ${detail}` : ""}`);
	}

	/** Run JS in the webview (must `return` a value). */
	js<T = unknown>(script: string): Promise<T> {
		return this.bridge.executeJs(script) as Promise<T>;
	}

	/** Invoke a backend Tauri command and await its result. */
	invoke<T = unknown>(cmd: string, args: Record<string, unknown> = {}, timeoutMs = 90_000): Promise<T> {
		return this.bridge.invokeAndWait(cmd, args, timeoutMs, `${this.name}-${this.sceneIdx}`) as Promise<T>;
	}

	/** Open the Settings dialog to a section via the shell event bus. */
	openSettings(section: string): Promise<unknown> {
		return this.js(
			`window.dispatchEvent(new CustomEvent("helmor:open-settings",{detail:{section:${JSON.stringify(section)}}})); return "ok";`,
		);
	}

	/** Close any open dialog (Escape). */
	closeDialog(): Promise<unknown> {
		return this.js(`document.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true})); return "esc";`);
	}

	/** Click the first element matching a CSS selector. Returns whether it hit. */
	click(selector: string): Promise<boolean> {
		return this.js<boolean>(
			`var el=document.querySelector(${JSON.stringify(selector)}); if(el){el.click(); return true;} return false;`,
		);
	}

	/** Wait until `selector` exists (or timeout). */
	async waitFor(selector: string, timeoutMs = 15_000): Promise<boolean> {
		const deadline = Date.now() + timeoutMs;
		while (Date.now() < deadline) {
			const hit = await this.js<boolean>(
				`return !!document.querySelector(${JSON.stringify(selector)});`,
			);
			if (hit) return true;
			await Bun.sleep(300);
		}
		return false;
	}

	sleep(ms: number): Promise<void> {
		return Bun.sleep(ms);
	}

	/** Capture the current window state as a captioned scene clip. */
	async scene(spec: SceneSpec): Promise<void> {
		const idx = String(this.sceneIdx++).padStart(2, "0");
		const out = `${this.outDir}/scene-${idx}.mp4`;
		this.log(`scene ${idx}: ${spec.caption}`);
		const p = Bun.spawn(
			[
				"bun",
				`${ROOT}/scripts/capture-scene.ts`,
				PROC,
				String(spec.record ?? 2),
				String(spec.hold ?? 4),
				out,
				spec.caption,
				SCALE_W,
			],
			{ stderr: "pipe", stdout: "pipe" },
		);
		if ((await p.exited) !== 0) {
			throw new Error(`capture-scene failed: ${await new Response(p.stderr).text()}`);
		}
		this.scenes.push(out);
	}

	/** Concatenate scenes → master.mp4 + master.gif, write result.json. */
	async finish(extra: Record<string, unknown> = {}): Promise<boolean> {
		const passed = this.assertions.every((a) => a.ok);
		await Bun.write(
			`${this.outDir}/result.json`,
			JSON.stringify(
				{ scenario: this.name, startedAt: new Date(this.t0).toISOString(), passed, assertions: this.assertions, ...extra },
				null,
				2,
			),
		);
		if (this.scenes.length > 0) {
			const p = Bun.spawn(["bun", `${ROOT}/scripts/finish-tape.ts`, this.outDir, ...this.scenes], {
				stderr: "inherit",
				stdout: "inherit",
			});
			await p.exited;
		}
		this.bridge.close();
		this.log(`finished; passed=${passed}`);
		return passed;
	}
}
