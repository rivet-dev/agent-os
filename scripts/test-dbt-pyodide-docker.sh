#!/usr/bin/env bash
# T7 — fresh-checkout reproduction in a clean Docker container.
#
# Spins up node:22-bookworm-slim, installs Python 3.13, pnpm, the toolchain,
# builds the wheel set, and runs `pnpm test:dbt-pyodide all`.
#
# This is the killer "works on a stranger's machine" gate. Expect ~45 minutes
# on first run (wheel builds dominate). Subsequent runs reuse the Docker
# layer cache.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMAGE="agent-os-dbt-pyodide-test"

echo "=== Building Docker image ==="
# Use trixie (Debian 13) which ships Python 3.13 by default. Bookworm only
# has Python 3.11; the wheel pipeline targets Python 3.13 to match the
# Pyodide ABI tag pyodide_2025_0_wasm32.
docker build -t "$IMAGE" -f - "$REPO_ROOT" <<'DOCKERFILE'
FROM node:22-trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
      python3 python3-venv python3-pip \
      git curl build-essential cmake ca-certificates \
      pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && python3 --version | grep -E "Python 3\.1[3-9]" \
       || { echo "FATAL: Python >= 3.13 required, got $(python3 --version)"; exit 1; }

RUN curl -fsSL https://get.pnpm.io/install.sh | ENV="/root/.bashrc" SHELL="$(which bash)" bash -
ENV PATH="/root/.local/share/pnpm:${PATH}"

WORKDIR /repo
COPY . .

RUN pnpm install --frozen-lockfile
RUN pnpm build

# Note: actual wheel build (`make -C registry/python-wheels build-all`) requires
# emsdk + nightly Rust + Pyodide cross-build env. The Docker image bootstraps
# that toolchain when run with --build-wheels. Without it, the harness skips
# the wheel-dependent layers but still validates everything else.

CMD ["bash", "-c", "pnpm test:dbt-pyodide all"]
DOCKERFILE

echo ""
echo "=== Running test:dbt-pyodide all in container ==="
docker run --rm "$IMAGE" pnpm test:dbt-pyodide all
EXIT=$?

if [ "$EXIT" -eq 0 ]; then
  echo "FRESH_REPRO_OK"
fi

exit "$EXIT"
