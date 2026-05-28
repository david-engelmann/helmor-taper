#!/usr/bin/env bun
// probe-remote-port-forward.ts
//
// Headless confirmation of remote port forwarding: starts a tiny HTTP
// server in the container at a known port, asks Helmor to forward a
// local port → container port via `ssh -O forward` over the existing
// SSH control master, fetches the local URL, and asserts the response
// body came from the container's service (not anything listening on
// localhost outside Helmor's control). Stops the forward + kills the
// server in cleanup.

import { Bridge } from "./mcp-bridge.ts";

const NAME = process.env.RUNTIME_NAME ?? "docker-linux-arm64";
const CONTAINER = process.env.CONTAINER ?? "helmor-test-linux-arm64";
// Pick uncommon ports so the test won't collide with anything else.
const REMOTE_PORT = Number(process.env.REMOTE_PORT ?? "47931");
const LOCAL_PORT = Number(process.env.LOCAL_PORT ?? "47932");
const MARKER = `PORTFWD_PROOF_${Date.now()}`;

const b = new Bridge();
await b.connect();
const inv = <T = unknown>(c: string, a: Record<string, unknown> = {}, t = 60_000) =>
	b.invokeAndWait(c, a, t, `pf-${c}`) as Promise<T>;

// Start a tiny HTTP server on the container that always returns MARKER.
// Background it via `setsid` so it survives the `docker exec` exit.
const inlineServer = [
	"python3 -c",
	`'import http.server, socket; s=http.server.HTTPServer(("0.0.0.0", ${REMOTE_PORT}), type("H", (http.server.BaseHTTPRequestHandler,), {"do_GET": lambda self: (self.send_response(200), self.send_header("content-type", "text/plain"), self.end_headers(), self.wfile.write(b"${MARKER}"))[0], "log_message": lambda *a, **k: None})); s.serve_forever()'`,
].join(" ");
const startServer = Bun.spawn([
	"docker", "exec", "-u", "e2e", "-d", CONTAINER, "sh", "-c",
	`setsid ${inlineServer} > /tmp/pyhttp.log 2>&1 &`,
]);
await startServer.exited;
// Give the server a moment to bind.
await Bun.sleep(800);
console.error(`✓ test server listening on container :${REMOTE_PORT}`);

// Confirm the server is reachable INSIDE the container before forwarding
// (sanity — if this fails the rest is moot).
const inner = Bun.spawn([
	"docker", "exec", CONTAINER, "sh", "-c",
	`echo "GET / HTTP/1.0\r\n\r\n" | python3 -c "import socket; s=socket.socket(); s.connect(('127.0.0.1', ${REMOTE_PORT})); s.sendall(b'GET / HTTP/1.0\\r\\n\\r\\n'); import sys; sys.stdout.buffer.write(s.recv(4096))"`,
], { stdout: "pipe" });
const innerBody = await new Response(inner.stdout).text();
const innerHit = innerBody.includes(MARKER);
if (!innerHit) {
	console.error(`✗ test server not responding inside container; got: ${innerBody.slice(0, 200)}`);
	process.exit(1);
}
console.error(`✓ test server responds with ${MARKER} inside the container`);

// Start the port forward via Helmor.
let started: { runtimeName: string; localPort: number; remotePort: number } | null = null;
try {
	started = (await inv("start_remote_port_forward", {
		runtimeName: NAME,
		localPort: LOCAL_PORT,
		remotePort: REMOTE_PORT,
		label: "probe",
	})) as { runtimeName: string; localPort: number; remotePort: number };
	console.error(`✓ start_remote_port_forward → ${started.localPort} → ${started.runtimeName}:${started.remotePort}`);
} catch (e) {
	console.error(`✗ start_remote_port_forward failed: ${String(e).slice(0, 200)}`);
	process.exit(1);
}

// Fetch via the LOCAL port — bytes should round-trip through SSH onto
// the container's listener and come back with our marker.
let outerBody = "";
let outerHit = false;
try {
	const res = await fetch(`http://127.0.0.1:${LOCAL_PORT}/`);
	outerBody = await res.text();
	outerHit = outerBody.includes(MARKER);
	console.error(`✓ localhost:${LOCAL_PORT} responded ${res.status} body="${outerBody.slice(0, 60)}"`);
} catch (e) {
	console.error(`✗ fetch via local port failed: ${String(e).slice(0, 160)}`);
}

// Confirm the manager's list reflects the active forward.
const listing = (await inv("list_remote_port_forwards")) as Array<{ runtimeName: string; localPort: number }>;
const listed = listing.some((e) => e.runtimeName === NAME && e.localPort === LOCAL_PORT);
console.error(`✓ list_remote_port_forwards includes the new entry: ${listed}`);

// Cleanup.
try {
	await inv("stop_remote_port_forward", { runtimeName: NAME, localPort: LOCAL_PORT });
	console.error("✓ stop_remote_port_forward");
} catch (e) {
	console.error(`(stop: ${String(e).slice(0, 80)})`);
}
const killServer = Bun.spawn([
	"docker", "exec", CONTAINER, "sh", "-c",
	`pkill -f 'http.server.HTTPServer.*${REMOTE_PORT}' 2>/dev/null || pkill -f 'HTTPServer' 2>/dev/null || true`,
]);
await killServer.exited;

b.close();
process.exit(outerHit && listed ? 0 : 1);
