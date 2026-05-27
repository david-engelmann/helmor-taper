#!/usr/bin/env bun
// capture-scene.ts
//
// Capture ONE captioned scene clip of the Helmor window and write it as a
// fixed-duration mp4 with the caption burned across the top.
//
// Why per-scene clips instead of one long timed-caption recording:
// ScreenCaptureKit only emits frames when the window content CHANGES, so a
// continuous recording with static holds compresses its timeline — timed
// `between(t,...)` captions then drift off the visuals. Capturing each
// visual beat as its own short clip and freeze-padding it to a known length
// sidesteps that entirely: the caption spans the whole clip, perfectly
// aligned, and live motion (connecting spinner, streaming output, typing)
// in the first seconds is preserved before the last frame freezes.
//
// Usage:
//   bun capture-scene.ts <proc> <recordSecs> <holdSecs> <out.mp4> <caption> [scaleW]
//
//   proc        window owner substring (e.g. "Helmor")
//   recordSecs  live ScreenCaptureKit capture length (catches motion)
//   holdSecs    final clip length; last frame freezes to fill the remainder
//   out.mp4     output path
//   caption     burned-in caption text
//   scaleW      output width (default: native)

const ROOT = new URL("..", import.meta.url).pathname;
const FONT = "/System/Library/Fonts/Supplemental/Arial.ttf";
const BAR_H = 64;
const BAR_ALPHA = 0.82;
const FONT_SIZE = 26;
const Y = 18;

function escapeDrawtext(s: string): string {
	return s
		.replace(/\\/g, "\\\\")
		.replace(/:/g, "\\:")
		.replace(/'/g, "’")
		.replace(/,/g, "\\,")
		.replace(/%/g, "\\%");
}

async function run(cmd: string[]): Promise<void> {
	const p = Bun.spawn(cmd, { stderr: "pipe", stdout: "pipe" });
	if ((await p.exited) !== 0) {
		const err = await new Response(p.stderr).text();
		throw new Error(`${cmd[0]} failed: ${err.slice(-700)}`);
	}
}

const [proc, recArg, holdArg, outMp4, caption, scaleArg] = Bun.argv.slice(2);
if (!proc || !recArg || !holdArg || !outMp4 || caption === undefined) {
	console.error("usage: capture-scene.ts <proc> <recordSecs> <holdSecs> <out.mp4> <caption> [scaleW]");
	process.exit(2);
}
const recordSecs = Number(recArg);
const holdSecs = Number(holdArg);
const tmpMov = `${outMp4}.raw.mov`;

// 1) live window capture (ScreenCaptureKit, window-buffer scoped)
await run(["swift", `${ROOT}/scripts/record-window.swift`, proc, String(recordSecs), tmpMov]);

// 2) normalize to CFR, freeze-pad to holdSecs, burn caption, trim exact.
//    tpad clones the final frame far past holdSecs; -t trims to exact length.
const text = escapeDrawtext(caption);
const scale = scaleArg ? `scale=${scaleArg}:-2,` : "";
const vf =
	`fps=12,${scale}tpad=stop_mode=clone:stop_duration=30,` +
	`drawbox=y=0:w=iw:h=${BAR_H}:color=black@${BAR_ALPHA}:t=fill,` +
	`drawtext=fontfile=${FONT}:text='${text}':fontcolor=white:fontsize=${FONT_SIZE}:x=(w-text_w)/2:y=${Y}`;
await run([
	"ffmpeg", "-y", "-i", tmpMov,
	"-vf", vf, "-t", String(holdSecs),
	"-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", "-r", "12",
	outMp4,
]);
await Bun.spawn(["rm", "-f", tmpMov]).exited;
console.error(`scene: ${outMp4} (${holdSecs}s, caption=${JSON.stringify(caption)})`);
