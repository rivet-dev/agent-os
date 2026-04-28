#!/bin/bash
#
# Run Ralph inside Docker with host-relative resource caps.
# - Memory limit: 50% of host RAM
# - CPU limit: host CPU count minus 2, with a floor of 1
#
# Usage:
#   ./scripts/ralph/ralph-docker.sh
#
# This wrapper always runs:
#   --tool codex 300

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

DOCKER_ARGS=(
  run
  --rm
  -it
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

HAS_CODEX_CONFIG=0

if [[ -f "${HOST_CODEX_DIR}/config.toml" ]]; then
  HAS_CODEX_CONFIG=1
fi

if [[ -f "${HOST_CODEX_CONFIG_DIR}/config.toml" ]]; then
  HAS_CODEX_CONFIG=1
fi

if [[ "${HAS_CODEX_CONFIG}" -eq 0 ]]; then
  echo "Error: Codex config not found. Expected ~/.codex/config.toml or ~/.config/codex/config.toml on the host." >&2
  echo "The Ralph Codex flow requires the host config so --profile ralph-long can resolve inside Docker." >&2
  exit 1
fi

if [[ -d "${HOST_CODEX_DIR}" ]]; then
  DOCKER_ARGS+=(-v "${HOST_CODEX_DIR}:/tmp/host-codex:ro")
fi

if [[ -d "${HOST_CODEX_CONFIG_DIR}" ]]; then
  DOCKER_ARGS+=(-v "${HOST_CODEX_CONFIG_DIR}:/tmp/host-codex-config:ro")
fi

if [[ -d "${HOST_CLAUDE_CONFIG_DIR}" ]]; then
  DOCKER_ARGS+=(-v "${HOST_CLAUDE_CONFIG_DIR}:/tmp/host-claude-config:ro")
fi

if [[ -d "${HOST_AMP_DIR}" ]]; then
  DOCKER_ARGS+=(-v "${HOST_AMP_DIR}:/tmp/host-amp:ro")
fi

if [[ -n "${OPENAI_API_KEY:-}" ]]; then
  DOCKER_ARGS+=(-e "OPENAI_API_KEY=${OPENAI_API_KEY}")
fi

if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
  DOCKER_ARGS+=(-e "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}")
fi

if [[ -n "${AMP_API_KEY:-}" ]]; then
  DOCKER_ARGS+=(-e "AMP_API_KEY=${AMP_API_KEY}")
fi

echo "Running Ralph in Docker"
echo "  Image: ${IMAGE}"
echo "  Dockerfile: ${DOCKERFILE}"
echo "  Repo: ${REPO_ROOT}"
echo "  User: ${HOST_USER} (${HOST_UID}:${HOST_GID})"
echo "  Memory limit: ${MEM_LIMIT_BYTES} bytes (50% of host RAM)"
echo "  CPU limit: ${CPU_LIMIT} (${HOST_CPUS} host CPUs minus 2)"
echo "  Codex home: ${CONTAINER_HOME}"
echo "  Codex config: host config copied into writable container home"

docker build -t "${IMAGE}" -f "${DOCKERFILE}" "${SCRIPT_DIR}"

exec docker "${DOCKER_ARGS[@]}" "${IMAGE}" \
  -c '
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
      # Host-level AGENTS.md is only relevant for user-facing Codex sessions.
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

    bash scripts/ralph/ralph.sh --tool codex 300
  '
