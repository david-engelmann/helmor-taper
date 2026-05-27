#!/usr/bin/env bun
// finish-tape.ts
//
// Concatenate captioned scene clips into a single master.mp4 + master.gif.
// All scenes must share resolution / fps / pixfmt (capture-scene.ts
// normalizes to 12fps yuv420p; pass the same scale width to every scene).
//
// Usage:
//   bun finish-tape.ts <out-dir> <scene1.mp4> <scene2.mp4> ...
//
// Emits <out-dir>/master.mp4 and <out-dir>/master.gif.

async function run(cmd: string[]): Promise<void> {
	const p = Bun.spawn(cmd, { stderr: "pipe", stdout: "pipe" });
	if ((await p.exited) !== 0) {
		const err = await new Response(p.stderr).text();
		throw new Error(`${cmd[0]} failed: ${err.slice(-700)}`);
	}
}

const [outDir, ...scenes] = Bun.argv.slice(2);
if (!outDir || scenes.length === 0) {
	console.error("usage: finish-tape.ts <out-dir> <scene1.mp4> ...");
	process.exit(2);
}
const mp4 = `${outDir}/master.mp4`;
const gif = `${outDir}/master.gif`;

// concat demuxer resolves `file` paths relative to the LIST file's dir,
// so use absolute paths to avoid double-prefixing.
const { resolve } = await import("node:path");
const listPath = `${outDir}/.concat.txt`;
await Bun.write(listPath, scenes.map((s) => `file '${resolve(s)}'`).join("\n") + "\n");

// 1) concat (re-encode for safety — scene encoders may differ slightly)
await run([
	"ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", listPath,
	"-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", "-r", "12",
	mp4,
]);

// 2) gif via shared palette
const palette = `${outDir}/.palette.png`;
const vf = "fps=10,scale=640:-1:flags=lanczos";
await run(["ffmpeg", "-y", "-i", mp4, "-vf", `${vf},palettegen=stats_mode=diff`, palette]);
await run([
	"ffmpeg", "-y", "-i", mp4, "-i", palette,
	"-lavfi", `${vf}[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3`,
	gif,
]);
await Bun.spawn(["rm", "-f", palette, listPath]).exited;

const sz = (await Bun.file(gif).arrayBuffer()).byteLength;
console.error(`tape: ${mp4} + ${gif} (${scenes.length} scenes, gif ${(sz / 1024) | 0}KB)`);
