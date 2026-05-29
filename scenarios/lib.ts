// scenarios/lib.ts
//
// Shared helpers for helmor-taper scenarios. A scenario drives the live
// Helmor UI through the MCP bridge and records the window via
// ScreenCaptureKit. Two recording modes are supported:
//
//   - "scene" (the original, per-feature tapes): each call to
//     `tape.scene(...)` captures a short clip with a burned-in
//     caption, then `finish()` concatenates them. Good for granular
//     per-feature gifs where every caption is precisely aligned.
//
//   - "continuous" (the demo tape): `tape.startRecording(durationSec)`
//     launches ScreenCaptureKit once for the whole tape; subsequent
//     `scene(...)` calls are markers + sleeps (no recording overhead).
//     `finish()` remuxes the single .mov → .mp4 (browser-playable)
//     and converts that mp4 → .gif via macOS AVAssetImageGenerator
//     (sharp; no ffmpeg palette/dither blur, no tpad-freeze artifacts).
//     This is the warp-taper pattern.

import { Bridge } from "../scripts/mcp-bridge.ts";

const ROOT = new URL("..", import.meta.url).pathname;
export const PROC = process.env.PROC_NAME ?? "Helmor";
export const SCALE_W = process.env.SCALE_W ?? "1100";

export type SceneSpec = {
	/** caption burned across the top of this beat (scene mode) /
	 *  marker text logged for the timeline (continuous mode) */
	caption: string;
	/** live ScreenCaptureKit capture seconds (catches motion) */
	record?: number;
	/** total clip seconds; last frame freezes to fill */
	hold?: number;
};

export type Assertion = { name: string; ok: boolean; detail?: string };

export type ContinuousBeat = { t: number; caption: string };

export class Tape {
	readonly bridge = new Bridge();
	private readonly scenes: string[] = [];
	private sceneIdx = 0;
	readonly assertions: Assertion[] = [];
	readonly t0 = Date.now();
	// Continuous-mode state.
	private continuous: {
		proc: ReturnType<typeof Bun.spawn>;
		movPath: string;
		startedAt: number;
		gifFps: number;
		gifMaxWidth: number;
	} | null = null;
	readonly beats: ContinuousBeat[] = [];

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

	/** Start a single ScreenCaptureKit recording that runs in the
	 *  background for `durationSec`. In continuous mode, subsequent
	 *  `scene(...)` calls become markers + sleeps — no per-scene
	 *  recording. The recording is converted to a sharp mp4 + gif on
	 *  `finish()`. Sizing the duration: budget the sum of `hold` values
	 *  you'll pass to `scene()` plus a 1–2 s buffer at each end.
	 *
	 *  `gifFps` / `gifMaxWidth` tune the mp4→gif step (warp-taper's
	 *  defaults are 5fps / 720px, which is what we use). */
	async startRecording(
		durationSec: number,
		opts: { gifFps?: number; gifMaxWidth?: number } = {},
	): Promise<void> {
		if (this.continuous) throw new Error("recording already started");
		await Bun.write(this.outDir + "/.placeholder", ""); // ensure dir
		const movPath = `${this.outDir}/master.mov`;
		// `record-window.swift` writes to stdout via stderr-logging; the
		// .mov path is positional. We pipe stderr to a file for debugging
		// but don't block on it.
		const proc = Bun.spawn(
			[
				"swift",
				`${ROOT}/scripts/record-window.swift`,
				PROC,
				String(durationSec),
				movPath,
			],
			{ stderr: "pipe", stdout: "pipe" },
		);
		this.continuous = {
			proc,
			movPath,
			startedAt: Date.now(),
			gifFps: opts.gifFps ?? 5,
			gifMaxWidth: opts.gifMaxWidth ?? 720,
		};
		this.log(`recording ${durationSec}s → ${movPath}`);
		// Give the recorder ~1.5s to acquire the window buffer before
		// the scenario starts driving the UI. ScreenCaptureKit's first
		// frame can take a moment.
		await Bun.sleep(1500);
	}

	/** Mark a beat in the tape's timeline. In continuous mode this
	 *  logs the beat + sleeps for the hold duration (so the recorder
	 *  captures the right moment). In scene mode it captures a
	 *  per-scene clip with a burned-in caption as before. */
	async scene(spec: SceneSpec): Promise<void> {
		if (this.continuous) {
			const t = (Date.now() - this.continuous.startedAt) / 1000;
			this.beats.push({ t, caption: spec.caption });
			this.log(`@${t.toFixed(1)}s — ${spec.caption}`);
			await Bun.sleep((spec.hold ?? 4) * 1000);
			return;
		}
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

	/** Concatenate scenes → master.mp4 + master.gif (scene mode), or
	 *  remux .mov → .mp4 → .gif (continuous mode). Writes result.json
	 *  + a beats.json timeline in continuous mode so the markdown
	 *  around the embed can list "what's happening at each timestamp".
	 */
	async finish(extra: Record<string, unknown> = {}): Promise<boolean> {
		const passed = this.assertions.every((a) => a.ok);
		await Bun.write(
			`${this.outDir}/result.json`,
			JSON.stringify(
				{ scenario: this.name, startedAt: new Date(this.t0).toISOString(), passed, assertions: this.assertions, beats: this.beats, ...extra },
				null,
				2,
			),
		);
		if (this.continuous) {
			// Wait for ScreenCaptureKit to finish writing the .mov.
			// The Swift process exits after its capture duration; we
			// gave the recorder a head-start sleep, so the recording
			// MAY still be wrapping up the encode tail.
			await this.continuous.proc.exited;
			const mov = this.continuous.movPath;
			const mp4 = mov.replace(/\.mov$/, ".mp4");
			const gif = mov.replace(/\.mov$/, ".gif");
			const { gifFps, gifMaxWidth } = this.continuous;
			// Remux mov → mp4 (no re-encode; cheap container swap).
			const mp4Proc = Bun.spawn(
				["swift", `${ROOT}/scripts/mov-to-mp4.swift`, mov, mp4],
				{ stderr: "inherit", stdout: "inherit" },
			);
			if ((await mp4Proc.exited) !== 0) throw new Error("mov-to-mp4 failed");
			// mp4 → gif via AVAssetImageGenerator (sharp; no ffmpeg
			// palette / dither artifacts; no tpad freeze frames).
			const gifProc = Bun.spawn(
				[
					"swift",
					`${ROOT}/scripts/mp4-to-gif.swift`,
					mp4,
					gif,
					String(gifFps),
					String(gifMaxWidth),
				],
				{ stderr: "inherit", stdout: "inherit" },
			);
			if ((await gifProc.exited) !== 0) throw new Error("mp4-to-gif failed");
			this.log(`continuous tape: ${mov} → ${mp4} → ${gif}`);
		} else if (this.scenes.length > 0) {
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
