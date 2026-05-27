#!/usr/bin/env bash
#
# record-remote-runner.sh — the helmor-taper end-to-end orchestrator.
#
# Brings up the Dockerized Linux remote, ensures the SSH config is in
# place, then records the Helmor window with ScreenCaptureKit WHILE the
# remote-runner scenario drives the live UI over the MCP bridge. Emits a
# PR-ready bundle (mov / mp4 / gif + result.json + a README) under TAPE_DIR.
#
# Preconditions:
#   - Helmor dev app already running (`bun run dev` with the MCP bridge on
#     :9223). We do NOT launch it here — building + first-run onboarding
#     are environment-specific; the bridge must simply be reachable.
#   - macOS with Screen Recording permission granted to this terminal.
#
# Env overrides:
#   HELMOR_SOURCE   helmor checkout (default ~/personal/helmor)
#   HOST_ALIAS      ssh-config alias            (default helmor-taper-arm64)
#   SERVICE         docker compose service      (default helmor-test-linux-arm64)
#   PORT            host ssh port               (default 2223)
#   RUNTIME_NAME    UI label                    (default docker-linux-arm64)
#   DURATION_S      recording length            (default 22)
#   TAPE_DIR        output dir                  (default tapes/remote-runner)

set -euo pipefail

HELMOR_SOURCE="${HELMOR_SOURCE:-$HOME/personal/helmor}"
HOST_ALIAS="${HOST_ALIAS:-helmor-taper-arm64}"
SERVICE="${SERVICE:-helmor-test-linux-arm64}"
PORT="${PORT:-2223}"
RUNTIME_NAME="${RUNTIME_NAME:-docker-linux-arm64}"
REMOTE_BINARY="${REMOTE_BINARY:-/home/e2e/.helmor/server/helmor-server}"
DURATION_S="${DURATION_S:-22}"
PROC_NAME="${PROC_NAME:-Helmor}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAPE_DIR="${TAPE_DIR:-${ROOT}/tapes/remote-runner}"
COMPOSE="${HELMOR_SOURCE}/src-tauri/tests/docker-e2e/compose.yml"
FIXTURES="${HELMOR_SOURCE}/src-tauri/tests/docker-e2e/fixtures"
IDENTITY="${FIXTURES}/id_e2e"

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }

[ "$(uname -s)" = "Darwin" ] || { echo "macOS only (ScreenCaptureKit)."; exit 1; }
mkdir -p "${TAPE_DIR}"

# ── Preflight: docker remote + ssh config + bridge reachability ────────
step "Bring up the Dockerized Linux remote (${SERVICE})"
docker compose -f "${COMPOSE}" up -d "${SERVICE}"
for i in $(seq 1 60); do
	st="$(docker inspect -f '{{.State.Health.Status}}' "${SERVICE}" 2>/dev/null || echo none)"
	[ "${st}" = "healthy" ] && break
	sleep 1
done
echo "container health: $(docker inspect -f '{{.State.Health.Status}}' "${SERVICE}")"

step "Write the ~/.ssh/config block for ${HOST_ALIAS} (:${PORT})"
chmod 600 "${IDENTITY}"
bash "${ROOT}/scripts/ssh-config.sh" add "${HOST_ALIAS}" "${PORT}" "${IDENTITY}"
ssh "${HOST_ALIAS}" "${REMOTE_BINARY} --version" | sed 's/^/  remote daemon: /'

step "Confirm the Helmor MCP bridge is reachable"
bun "${ROOT}/scripts/mcp-bridge.ts" ping >/dev/null \
	|| { echo "MCP bridge not reachable on :9223 — is 'bun run dev' running (debug build)?"; exit 1; }
echo "  bridge OK"

# ── Record + drive (in parallel) ───────────────────────────────────────
MOV="${TAPE_DIR}/master.mov"
MP4="${TAPE_DIR}/master.mp4"
GIF="${TAPE_DIR}/master.gif"
rm -f "${MOV}" "${MP4}" "${GIF}"

step "Record the Helmor window for ${DURATION_S}s while driving the scenario"
swift "${ROOT}/scripts/record-window.swift" "${PROC_NAME}" "${DURATION_S}" "${MOV}" 2>"${TAPE_DIR}/record.log" &
REC_PID=$!
sleep 1  # let the capture stream spin up before the first scene

set +e
HOST_ALIAS="${HOST_ALIAS}" RUNTIME_NAME="${RUNTIME_NAME}" REMOTE_BINARY="${REMOTE_BINARY}" \
	ARTIFACT_DIR="${TAPE_DIR}" \
	bun "${ROOT}/scenarios/remote-runner.ts" 2>"${TAPE_DIR}/scenario.log"
SCENARIO_RC=$?
set -e
echo "  scenario exit: ${SCENARIO_RC} (see ${TAPE_DIR}/scenario.log)"

wait "${REC_PID}" || true
[ -s "${MOV}" ] || { echo "recorder produced no bytes — Screen Recording permission?"; exit 1; }
echo "  wrote ${MOV} ($(wc -c <"${MOV}" | tr -d ' ') bytes)"

# ── Convert for the web (mp4) + markdown (gif) ─────────────────────────
step "Convert mov → mp4 → gif"
swift "${ROOT}/scripts/mov-to-mp4.swift" "${MOV}" "${MP4}" 2>>"${TAPE_DIR}/record.log" || echo "  (mp4 convert skipped)"
swift "${ROOT}/scripts/mp4-to-gif.swift" "${MP4}" "${GIF}" 6 720 2>>"${TAPE_DIR}/record.log" || echo "  (gif convert skipped)"

# ── Bundle README ──────────────────────────────────────────────────────
step "Write bundle README"
PASSED="$(bun -e 'const r=await Bun.file(process.argv[1]).json().catch(()=>({passed:false})); console.log(r.passed?"PASS":"FAIL")' "${TAPE_DIR}/result.json" 2>/dev/null || echo "?")"
{
	echo "# Tape: remote-runner"
	echo
	echo "Helmor desktop connecting to a Dockerized Linux host running"
	echo "\`helmor-server\` over SSH, recorded against a live debug build."
	echo
	echo "- **Result:** ${PASSED}"
	echo "- **Host:** \`${HOST_ALIAS}\` (docker \`${SERVICE}\`, ssh :${PORT})"
	echo "- **Remote daemon:** \`${REMOTE_BINARY}\`"
	echo "- **Recorded:** $(date -u +%Y-%m-%dT%H:%M:%SZ)"
	echo
	echo '![remote-runner](master.gif)'
	echo
	echo '<video src="master.mp4" controls width="720"></video>'
	echo
	echo "## Artifacts"
	echo
	echo "| File | What |"
	echo "|---|---|"
	echo "| \`master.mov\` | ScreenCaptureKit window capture (source) |"
	echo "| \`master.mp4\` | browser-friendly H.264 |"
	echo "| \`master.gif\` | markdown-embeddable loop |"
	echo "| \`result.json\` | RuntimeHealth + programmatic assertions |"
	echo "| \`scenario.log\` / \`record.log\` | per-stage logs |"
	echo
	echo "## Assertions"
	echo
	echo '```json'
	cat "${TAPE_DIR}/result.json" 2>/dev/null | bun -e 'const r=await Bun.stdin.json().catch(()=>null); if(r) console.log(JSON.stringify({connectMs:r.connectMs, health:r.health, assertions:r.assertions, passed:r.passed}, null, 2)); else console.log("{}")' 2>/dev/null || echo "{}"
	echo '```'
} > "${TAPE_DIR}/README.md"

step "Done"
echo "Bundle: ${TAPE_DIR}"
ls -la "${TAPE_DIR}"
