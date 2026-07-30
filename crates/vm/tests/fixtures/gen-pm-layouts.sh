#!/usr/bin/env bash
# Generate a clean, isolated node_modules layout per package manager, using
# Docker containers so each install runs with fresh caches/stores and no host
# pollution. Each layout is a workspace root + an `app` subpackage depending on
# `is-odd` (which pulls the transitive `is-number`), reproducing the real
# monorepo case where a package's store escapes the mounted project dir.
#
# Usage:
#   crates/sidecar/tests/fixtures/gen-pm-layouts.sh [OUT_DIR]
# then point the layout test at it:
#   PM_LAYOUTS_DIR=$OUT_DIR cargo test -p agent-os-client --test module_layout_e2e -- --nocapture
#
# The resulting per-PM store signatures (what the sidecar detector keys on):
#   pnpm (isolated)      app/node_modules/<dep> -> ../../node_modules/.pnpm/...
#   bun (workspace)      app/node_modules/<dep> -> ../../node_modules/.bun/...
#   yarn (nodeLinker:pnpm) app/node_modules/<dep> -> ../../node_modules/.store/...
#   yarn (pnp, default)  .pnp.cjs + .yarn/cache, NO node_modules
#   npm / yarn-nm / pnpm-hoisted   flat (hoisted to root, no store) -> work
set -uo pipefail

OUT="${1:-$(mktemp -d /tmp/pm-layouts.XXXXXX)}"
mkdir -p "$OUT"
echo "output: $OUT"

NODE_IMG=node:22-bookworm-slim
BUN_IMG=oven/bun:1-slim
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0

scaffold='
mkdir -p app
cat > package.json <<JSON
{ "name": "root", "private": true, "version": "1.0.0", "workspaces": ["app"] }
JSON
cat > app/package.json <<JSON
{ "name": "app", "version": "1.0.0", "dependencies": { "is-odd": "3.0.1" } }
JSON
'

run() { # name image script
  local name="$1" image="$2" script="$3"
  mkdir -p "$OUT/$name"
  printf '%-16s ' "$name"
  if docker run --rm -v "$OUT/$name:/work" -w /work -e COREPACK_ENABLE_DOWNLOAD_PROMPT=0 \
      "$image" bash -lc "$scaffold $script" > "$OUT/$name.log" 2>&1; then
    echo "ok"
  else
    echo "FAILED (see $OUT/$name.log)"
  fi
}

run npm           "$NODE_IMG" 'npm install --silent'
run pnpm-isolated "$NODE_IMG" 'echo "packages: [app]" > pnpm-workspace.yaml; corepack enable >/dev/null 2>&1; corepack pnpm install --config.store-dir=/work/.store --silent'
run pnpm-hoisted  "$NODE_IMG" 'echo "packages: [app]" > pnpm-workspace.yaml; printf "node-linker=hoisted\n" > .npmrc; corepack enable >/dev/null 2>&1; corepack pnpm install --config.store-dir=/work/.store --silent'
run yarn-pnp      "$NODE_IMG" 'corepack enable >/dev/null 2>&1; corepack yarn set version stable >/dev/null 2>&1; corepack yarn install >/dev/null 2>&1'
run yarn-nm       "$NODE_IMG" 'printf "nodeLinker: node-modules\n" > .yarnrc.yml; corepack enable >/dev/null 2>&1; corepack yarn set version stable >/dev/null 2>&1; corepack yarn install >/dev/null 2>&1'
run yarn-pnpm     "$NODE_IMG" 'printf "nodeLinker: pnpm\n" > .yarnrc.yml; corepack enable >/dev/null 2>&1; corepack yarn set version stable >/dev/null 2>&1; corepack yarn install >/dev/null 2>&1'
run bun           "$BUN_IMG"  'bun install >/dev/null 2>&1'

echo ""
echo "PM_LAYOUTS_DIR=$OUT"
