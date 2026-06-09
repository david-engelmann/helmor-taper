#!/usr/bin/env bash
#
# record-remote-runner.sh — the helmor-taper end-to-end orchestrator.
#
# Brings up the Dockerized Linux remote, writes the ssh-config block,
# confirms the Helmor MCP bridge is reachable, then runs the
# remote-runner scenario via the Rust `taper` binary. The scenario
# itself owns the ScreenCaptureKit recording + the mov→mp4→gif
# conversion (TS scaffolding was retired in Phase R6 of the
# TS→Rust migration).
#
# Preconditions:
#   - Helmor dev app running (`bun run dev` with the MCP bridge on
#     :9223). We do NOT launch it here — the bridge must be reachable.
#   - macOS with Screen Recording permission granted to this terminal.
#   - The `taper` binary built (`cargo build --release`).
#
# Env overrides:
#   HELMOR_SOURCE   helmor checkout (default ~/personal/helmor)
#   HOST_ALIAS      ssh-config alias            (default helmor-taper-arm64)
#   SERVICE         docker compose service      (default helmor-test-linux-arm64)
#   PORT            host ssh port               (default 2223)
#   RUNTIME_NAME    UI label                    (default docker-linux-arm64)
#   TAPE_DIR        output dir                  (default tapes/remote-runner)
#   TAPER_BIN       path to the taper binary    (default ${ROOT}/target/release/taper)

set -euo pipefail

HELMOR_SOURCE="${HELMOR_SOURCE:-$HOME/personal/helmor}"
HOST_ALIAS="${HOST_ALIAS:-helmor-taper-arm64}"
SERVICE="${SERVICE:-helmor-test-linux-arm64}"
PORT="${PORT:-2223}"
RUNTIME_NAME="${RUNTIME_NAME:-docker-linux-arm64}"
REMOTE_BINARY="${REMOTE_BINARY:-/home/e2e/.helmor/server/helmor-server}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAPE_DIR="${TAPE_DIR:-${ROOT}/tapes/remote-runner}"
COMPOSE="${HELMOR_SOURCE}/src-tauri/tests/docker-e2e/compose.yml"
FIXTURES="${HELMOR_SOURCE}/src-tauri/tests/docker-e2e/fixtures"
IDENTITY="${FIXTURES}/id_e2e"
TAPER_BIN="${TAPER_BIN:-${ROOT}/target/release/taper}"

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }

[ "$(uname -s)" = "Darwin" ] || { echo "macOS only (ScreenCaptureKit)."; exit 1; }
mkdir -p "${TAPE_DIR}"

# ── Build the taper binary if it doesn't exist yet ────────────────────
if [ ! -x "${TAPER_BIN}" ]; then
	step "Build the taper binary (release)"
	(cd "${ROOT}" && cargo build --release --bin taper)
fi

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
"${TAPER_BIN}" ping >/dev/null \
	|| { echo "MCP bridge not reachable on :9223 — is 'bun run dev' running (debug build)?"; exit 1; }
echo "  bridge OK"

# ── Run the scenario (records + drives + converts in one process) ──────
step "Run the remote-runner scenario via taper"
set +e
HOST_ALIAS="${HOST_ALIAS}" RUNTIME_NAME="${RUNTIME_NAME}" REMOTE_BINARY="${REMOTE_BINARY}" \
	TAPE_DIR="${TAPE_DIR}" \
	"${TAPER_BIN}" scenario remote-runner 2>"${TAPE_DIR}/scenario.log"
SCENARIO_RC=$?
set -e
echo "  scenario exit: ${SCENARIO_RC} (see ${TAPE_DIR}/scenario.log)"

# ── Bundle README ──────────────────────────────────────────────────────
step "Write bundle README"
if command -v jq >/dev/null 2>&1; then
	PASSED="$(jq -r 'if .passed then "PASS" else "FAIL" end' "${TAPE_DIR}/result.json" 2>/dev/null || echo "?")"
else
	PASSED="?"
fi
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
	echo "| \`scenario.log\` | scenario run log |"
} > "${TAPE_DIR}/README.md"

step "Done"
echo "Bundle: ${TAPE_DIR}"
ls -la "${TAPE_DIR}"
