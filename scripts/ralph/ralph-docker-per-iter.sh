#!/bin/bash
#
# Per-iteration Ralph wrapper. Like scripts/ralph/ralph-docker.sh, but each
# Ralph iteration runs in its own fresh Docker container instead of looping
# inside one long-lived container. This breaks the OOM-loop where leaked
# sidecars / V8 isolates accumulate inside a single container until the
# 31 GiB cgroup limit kills it.
#
# Codex only. Usage:
#   ./scripts/ralph/ralph-docker-per-iter.sh [max_iterations]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

IMAGE="ralph-runner"
WORKDIR_IN_CONTAINER="/workspace"
HOST_UID="$(id -u)"
HOST_GID="$(id -g)"
HOST_USER="${USER:-agent}"
CONTAINER_HOME="/var/tmp/ralph-home"
DOCKERFILE="${SCRIPT_DIR}/Dockerfile"
HOST_CODEX_DIR="${HOME}/.codex"
HOST_CODEX_CONFIG_DIR="${HOME}/.config/codex"
HOST_CLAUDE_CONFIG_DIR="${HOME}/.config/claude"
HOST_AMP_DIR="${HOME}/.amp"
CODEX_STREAM_DIR="${SCRIPT_DIR}/codex-streams"

MAX_ITERATIONS="${1:-300}"
if ! [[ "${MAX_ITERATIONS}" =~ ^[0-9]+$ ]]; then
  echo "Error: max_iterations must be a positive integer (got '${MAX_ITERATIONS}')." >&2
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "Error: docker is required but was not found in PATH." >&2
  exit 1
fi

if [[ ! -f "${DOCKERFILE}" ]]; then
  echo "Error: Ralph Dockerfile not found at ${DOCKERFILE}." >&2
  exit 1
fi

if [[ ! -r /proc/meminfo ]]; then
  echo "Error: /proc/meminfo is required to detect host RAM." >&2
  exit 1
fi

TOTAL_MEM_KB="$(awk '/MemTotal:/ { print $2; exit }' /proc/meminfo)"
if [[ -z "$TOTAL_MEM_KB" || ! "$TOTAL_MEM_KB" =~ ^[0-9]+$ ]]; then
  echo "Error: failed to parse total host RAM from /proc/meminfo." >&2
  exit 1
fi

MEM_LIMIT_KB=$((TOTAL_MEM_KB / 2))
MEM_LIMIT_BYTES=$((MEM_LIMIT_KB * 1024))

HOST_CPUS="$(nproc)"
if [[ -z "$HOST_CPUS" || ! "$HOST_CPUS" =~ ^[0-9]+$ ]]; then
  echo "Error: failed to determine host CPU count." >&2
  exit 1
fi

CPU_LIMIT=$((HOST_CPUS - 2))
if (( CPU_LIMIT < 1 )); then
  CPU_LIMIT=1
fi

HAS_CODEX_CONFIG=0
if [[ -f "${HOST_CODEX_DIR}/config.toml" ]]; then
  HAS_CODEX_CONFIG=1
fi
if [[ -f "${HOST_CODEX_CONFIG_DIR}/config.toml" ]]; then
  HAS_CODEX_CONFIG=1
fi
if [[ "${HAS_CODEX_CONFIG}" -eq 0 ]]; then
  echo "Error: Codex config not found. Expected ~/.codex/config.toml or ~/.config/codex/config.toml on the host." >&2
  exit 1
fi

mkdir -p "${CODEX_STREAM_DIR}"

# Resume step numbering: highest existing step-N.log + 1
NEXT_STEP=1
shopt -s nullglob
for f in "${CODEX_STREAM_DIR}"/step-*.log; do
  base="${f##*/}"
  num="${base#step-}"
  num="${num%.log}"
  if [[ "$num" =~ ^[0-9]+$ ]] && (( num >= NEXT_STEP )); then
    NEXT_STEP=$((num + 1))
  fi
done
shopt -u nullglob

# Build the image (cheap if cached).
docker build -t "${IMAGE}" -f "${DOCKERFILE}" "${SCRIPT_DIR}"

DOCKER_ARGS_BASE=(
  run
  --rm
  --entrypoint bash
  --user "${HOST_UID}:${HOST_GID}"
  -e "HOME=${CONTAINER_HOME}"
  -e "USER=${HOST_USER}"
  -e "LOGNAME=${HOST_USER}"
  --memory="${MEM_LIMIT_BYTES}b"
  --memory-swap="${MEM_LIMIT_BYTES}b"
  --cpus="${CPU_LIMIT}"
  -v "${REPO_ROOT}:${WORKDIR_IN_CONTAINER}"
  -w "${WORKDIR_IN_CONTAINER}"
)

if [[ -d "${HOST_CODEX_DIR}" ]]; then
  DOCKER_ARGS_BASE+=(-v "${HOST_CODEX_DIR}:/tmp/host-codex:ro")
fi
if [[ -d "${HOST_CODEX_CONFIG_DIR}" ]]; then
  DOCKER_ARGS_BASE+=(-v "${HOST_CODEX_CONFIG_DIR}:/tmp/host-codex-config:ro")
fi
if [[ -d "${HOST_CLAUDE_CONFIG_DIR}" ]]; then
  DOCKER_ARGS_BASE+=(-v "${HOST_CLAUDE_CONFIG_DIR}:/tmp/host-claude-config:ro")
fi
if [[ -d "${HOST_AMP_DIR}" ]]; then
  DOCKER_ARGS_BASE+=(-v "${HOST_AMP_DIR}:/tmp/host-amp:ro")
fi

if [[ -n "${OPENAI_API_KEY:-}" ]]; then
  DOCKER_ARGS_BASE+=(-e "OPENAI_API_KEY=${OPENAI_API_KEY}")
fi
if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
  DOCKER_ARGS_BASE+=(-e "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}")
fi
if [[ -n "${AMP_API_KEY:-}" ]]; then
  DOCKER_ARGS_BASE+=(-e "AMP_API_KEY=${AMP_API_KEY}")
fi

INLINE_SCRIPT='
set -euo pipefail
mkdir -p "$HOME/.config"

copy_tree_if_present() {
  local src="$1"
  local dest="$2"
  if [[ -e "$src" ]]; then
    mkdir -p "$(dirname "$dest")"
    cp -a "$src" "$dest"
  fi
}

if [[ -d /tmp/host-codex ]]; then
  mkdir -p "$HOME/.codex"
  copy_tree_if_present /tmp/host-codex/auth.json "$HOME/.codex/auth.json"
  copy_tree_if_present /tmp/host-codex/config.toml "$HOME/.codex/config.toml"
  copy_tree_if_present /tmp/host-codex/prompts "$HOME/.codex/prompts"
  copy_tree_if_present /tmp/host-codex/rules "$HOME/.codex/rules"
  copy_tree_if_present /tmp/host-codex/skills "$HOME/.codex/skills"
  copy_tree_if_present /tmp/host-codex/plugins "$HOME/.codex/plugins"
fi

if [[ -d /tmp/host-codex-config ]]; then
  mkdir -p "$HOME/.config/codex"
  copy_tree_if_present /tmp/host-codex-config/config.toml "$HOME/.config/codex/config.toml"
fi

if [[ -d /tmp/host-claude-config ]]; then
  mkdir -p "$HOME/.config/claude"
  cp -a /tmp/host-claude-config/. "$HOME/.config/claude/"
fi

if [[ -d /tmp/host-amp ]]; then
  mkdir -p "$HOME/.amp"
  cp -a /tmp/host-amp/. "$HOME/.amp/"
fi

CODEX_LAST_MSG=$(mktemp)
codex exec --profile ralph --dangerously-bypass-approvals-and-sandbox \
  -C /workspace/scripts/ralph -o "$CODEX_LAST_MSG" - \
  < /workspace/scripts/ralph/CODEX.md 2>&1 \
  | ts "[%Y-%m-%d %H:%M:%S]" \
  | tee "$STEP_STREAM_FILE" >/dev/null || true

if tail -n 20 "$CODEX_LAST_MSG" | grep -q "<promise>COMPLETE</promise>"; then
  rm -f "$CODEX_LAST_MSG"
  exit 99
fi
rm -f "$CODEX_LAST_MSG"
'

CURRENT_CID_FILE=""
on_signal() {
  if [[ -n "${CURRENT_CID_FILE}" && -f "${CURRENT_CID_FILE}" ]]; then
    local cid
    cid="$(cat "${CURRENT_CID_FILE}" 2>/dev/null || true)"
    if [[ -n "$cid" ]]; then
      docker kill "$cid" >/dev/null 2>&1 || true
    fi
  fi
  exit 130
}
trap on_signal INT TERM

echo "Running Ralph in Docker (per-iteration)"
echo "  Image: ${IMAGE}"
echo "  Dockerfile: ${DOCKERFILE}"
echo "  Repo: ${REPO_ROOT}"
echo "  User: ${HOST_USER} (${HOST_UID}:${HOST_GID})"
echo "  Memory limit: ${MEM_LIMIT_BYTES} bytes (50% of host RAM)"
echo "  CPU limit: ${CPU_LIMIT} (${HOST_CPUS} host CPUs minus 2)"
echo "  Codex home: ${CONTAINER_HOME}"
echo "  Codex config: host config copied into writable container home"
echo "  Stream dir: ${CODEX_STREAM_DIR}"
echo "  Starting at iteration: ${NEXT_STEP}"

RUN_START=$(date '+%Y-%m-%d %H:%M:%S')
echo "Starting Ralph - Tool: codex - Max iterations: ${MAX_ITERATIONS}"
echo "Run started: $RUN_START"

ITER_COUNT=0
RALPH_ITER="${NEXT_STEP}"
while (( ITER_COUNT < MAX_ITERATIONS )); do
  ITER_START=$(date '+%Y-%m-%d %H:%M:%S')
  STEP_STREAM_FILE="${CODEX_STREAM_DIR}/step-${RALPH_ITER}.log"

  echo ""
  echo "==============================================================="
  echo "  Ralph Iteration ${RALPH_ITER} (codex)"
  echo "  Started: ${ITER_START}"
  echo "==============================================================="
  echo "Codex stream: ${STEP_STREAM_FILE}"

  CURRENT_CID_FILE="$(mktemp -u)"
  DOCKER_ARGS=("${DOCKER_ARGS_BASE[@]}"
    --cidfile "${CURRENT_CID_FILE}"
    -e "STEP_STREAM_FILE=/workspace/scripts/ralph/codex-streams/step-${RALPH_ITER}.log"
    "${IMAGE}"
    -c "${INLINE_SCRIPT}")

  set +e
  docker "${DOCKER_ARGS[@]}"
  rc=$?
  set -e
  rm -f "${CURRENT_CID_FILE}"
  CURRENT_CID_FILE=""

  ITER_END=$(date '+%Y-%m-%d %H:%M:%S')
  ITER_DURATION=$(( $(date -d "${ITER_END}" +%s) - $(date -d "${ITER_START}" +%s) ))
  ITER_MINS=$((ITER_DURATION / 60))
  ITER_SECS=$((ITER_DURATION % 60))

  if (( rc == 99 )); then
    RUN_END=$(date '+%Y-%m-%d %H:%M:%S')
    RUN_DURATION=$(( $(date -d "${RUN_END}" +%s) - $(date -d "${RUN_START}" +%s) ))
    RUN_MINS=$((RUN_DURATION / 60))
    RUN_SECS=$((RUN_DURATION % 60))
    echo ""
    echo "Ralph completed all tasks!"
    echo "Completed at iteration ${RALPH_ITER}"
    echo "Iteration: ${ITER_MINS}m ${ITER_SECS}s"
    echo "Run started:  $RUN_START"
    echo "Run finished: $RUN_END (total: ${RUN_MINS}m ${RUN_SECS}s)"
    exit 0
  fi

  if (( rc != 0 )); then
    echo "Iteration ${RALPH_ITER} container exited ${rc} (likely OOM if 137). Continuing." >&2
  fi

  echo "Iteration ${RALPH_ITER} complete. Finished: ${ITER_END} (${ITER_MINS}m ${ITER_SECS}s)"

  RALPH_ITER=$((RALPH_ITER + 1))
  ITER_COUNT=$((ITER_COUNT + 1))
done

RUN_END=$(date '+%Y-%m-%d %H:%M:%S')
RUN_DURATION=$(( $(date -d "${RUN_END}" +%s) - $(date -d "${RUN_START}" +%s) ))
RUN_MINS=$((RUN_DURATION / 60))
RUN_SECS=$((RUN_DURATION % 60))
echo ""
echo "Ralph reached max iterations (${MAX_ITERATIONS}) without completing all tasks."
echo "Run started:  $RUN_START"
echo "Run finished: $RUN_END (total: ${RUN_MINS}m ${RUN_SECS}s)"
exit 1
