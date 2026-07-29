#!/usr/bin/env bash
set -euo pipefail

experiment_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH= cd -- "$experiment_dir/../.." && pwd)
install_dir=${GIGACODE_INSTALL_BIN_DIR:-"$HOME/.local/bin"}
install_root=${GIGACODE_INSTALL_ROOT:-"$HOME/.local/share/gigacode"}
destination="$install_dir/gigacode"
alias_destination="$install_dir/giga"
legacy_alias_destination="$install_dir/gc"

if ! command -v pnpm >/dev/null 2>&1; then
	echo "pnpm is required to install Gigacode globally." >&2
	exit 1
fi

mkdir -p "$install_dir" "$(dirname -- "$install_root")"
staging=$(mktemp -d "$(dirname -- "$install_root")/.gigacode-deploy.XXXXXX")
temporary=$(mktemp "$install_dir/.gigacode.XXXXXX")
cleanup() {
	rm -f "$temporary"
	rm -rf "$staging"
}
trap cleanup EXIT

# Repair a pnpm store entry whose downloaded OpenCode executable was removed
# after its lifecycle script had already been marked complete.
opencode_package="$experiment_dir/node_modules/opencode-ai"
if [[ -f "$opencode_package/postinstall.mjs" && ! -f "$opencode_package/bin/opencode.exe" ]]; then
	(cd "$opencode_package" && node postinstall.mjs)
fi

# Native AgentOS builds invoke packages/build-tools from Cargo build scripts,
# which is outside Gigacode's filtered pnpm dependency graph.
pnpm --dir "$repo_root" install

case "$(uname -s)" in
	Linux|Darwin) ;;
	*)
		echo "The Gigacode source installer supports Linux and macOS." >&2
		exit 1
		;;
esac
if [[ -n "${AGENTOS_SIDECAR_BIN:-}" ]]; then
	sidecar_source=$AGENTOS_SIDECAR_BIN
	if [[ ! -x "$sidecar_source" ]]; then
		echo "The configured AgentOS sidecar does not exist or is not executable." >&2
		exit 1
	fi
else
	cargo build --release \
		--manifest-path "$repo_root/Cargo.toml" \
		-p agentos-sidecar
	sidecar_source="$repo_root/target/release/agentos-sidecar"
fi

pnpm --dir "$repo_root" \
	--filter @rivet-dev/agentos-experiment-gigacode \
	deploy --legacy --prod "$staging"

mkdir -p "$staging/native"
cp "$sidecar_source" "$staging/native/agentos-sidecar"
chmod 0755 "$staging/native/agentos-sidecar"
mkdir -p "$staging/bin"
cp "$staging/node_modules/opencode-ai/bin/opencode.exe" "$staging/bin/opencode"
chmod 0755 "$staging/bin/opencode"
engine_source=$(find "$staging/node_modules/.pnpm" \
	-path '*/node_modules/@rivetkit/engine-cli-*/rivet-engine' \
	-type f -print -quit)
if [[ -z "$engine_source" ]]; then
	echo "Gigacode deployment did not contain the Rivet engine binary." >&2
	exit 1
fi
cp "$engine_source" "$staging/bin/rivet-engine"
chmod 0755 "$staging/bin/rivet-engine"
mkdir -p "$staging/software"
for package in \
	coreutils sed grep gawk findutils diffutils tar gzip ripgrep \
	claude-code codex opencode pi; do
	case "$package" in
		claude-code) local_package="$repo_root/software/claude/dist/package.aospkg" ;;
		codex) local_package="$repo_root/software/codex/dist/package.aospkg" ;;
		opencode) local_package="$repo_root/software/opencode/dist/package.aospkg" ;;
		pi) local_package="$repo_root/software/pi/dist/package.aospkg" ;;
		coreutils|sed|grep|gawk|findutils|diffutils|tar|gzip|ripgrep)
			local_package="$repo_root/software/$package/dist/package.aospkg"
			;;
		*) local_package="" ;;
	esac
	package_source=""
	if [[ -n "$local_package" && -f "$local_package" ]]; then
		package_source="$local_package"
	else
		package_source=$(find "$staging/node_modules/.pnpm" -path "*/node_modules/@agentos-software/$package/dist/package.aospkg" -print -quit)
	fi
	if [[ -z "$package_source" ]]; then
		echo "Gigacode source build is missing @agentos-software/$package package.aospkg" >&2
		exit 1
	fi
	cp "$package_source" "$staging/software/$package.aospkg"
done

if [[ ! -x "$staging/node_modules/.bin/tsx" || ! -f "$staging/node_modules/tsx/dist/loader.mjs" || ! -x "$staging/node_modules/.bin/opencode" || ! -f "$staging/gigacode.ts" ]]; then
	echo "Gigacode deployment did not contain its runtime entrypoint." >&2
	exit 1
fi
# The shared development machine may prune or rewrite generated node_modules
# trees. Rename the deployed root out of its watched location before taking a
# fast uncompressed snapshot, transform it back to node_modules in the archive,
# then compress the stable archive.
mv "$staging/node_modules" "$staging/runtime-tree"
snapshot_runtime() {
	tar -cf "$staging/runtime.tar" \
		-C "$staging" \
		--transform='s,^runtime-tree,node_modules,' \
		runtime-tree
}
if ! snapshot_runtime; then
	echo "Gigacode runtime changed during snapshot; retrying once after deploy settled" >&2
	rm -f "$staging/runtime.tar"
	snapshot_runtime
fi
gzip -1 "$staging/runtime.tar"
mv "$staging/runtime-tree" "$staging/node_modules"

backup="$install_root.previous.$$"
rm -rf "$backup"
if [[ -x "$destination" ]]; then
	"$destination" daemon stop >/dev/null 2>&1 || true
fi
if [[ -e "$install_root" ]]; then
	mv "$install_root" "$backup"
fi
mv "$staging" "$install_root"
rm -rf "$backup"

printf '#!/usr/bin/env bash
set -euo pipefail
install_root=%q
runtime_bin="$install_root/node_modules/.bin"
opencode_bin="${GIGACODE_OPENCODE_BIN:-$install_root/bin/opencode}"
api_port="${GIGACODE_PORT:-2468}"
api_endpoint="http://127.0.0.1:$api_port"
first="${1:-}"
case "$first" in
	daemon|debugger|models|shell|help|--help|-h|--version|-V) client_mode=0 ;;
	*) client_mode=1 ;;
esac
if [[ "$client_mode" == 1 && -x "$opencode_bin" ]] && \
	curl --silent --fail --max-time 0.25 "$api_endpoint/global/health" >/dev/null 2>&1; then
	if [[ "$first" == run ]]; then
		shift
		exec "$opencode_bin" run --attach "$api_endpoint/opencode" "$@"
	fi
	exec "$opencode_bin" attach "$api_endpoint/opencode" "$@"
fi
if [[ ! -x "$runtime_bin/tsx" || ! -f "$install_root/node_modules/tsx/dist/loader.mjs" || ! -x "$runtime_bin/opencode" ]]; then
	recovery=$(mktemp -d "$install_root/.runtime.XXXXXX")
	cleanup() { rm -rf "$recovery"; }
	trap cleanup EXIT
	tar -xzf "$install_root/runtime.tar.gz" -C "$recovery"
	rm -rf "$install_root/node_modules"
	mv "$recovery/node_modules" "$install_root/node_modules"
	rmdir "$recovery"
	trap - EXIT
fi
export AGENTOS_SIDECAR_BIN="${AGENTOS_SIDECAR_BIN:-$install_root/native/agentos-sidecar}"
export GIGACODE_SOFTWARE_DIR="${GIGACODE_SOFTWARE_DIR:-$install_root/software}"
export RIVET_ENGINE_BINARY="${RIVET_ENGINE_BINARY:-$install_root/bin/rivet-engine}"
export RIVET_ENGINE_BINARY_PATH="${RIVET_ENGINE_BINARY_PATH:-$RIVET_ENGINE_BINARY}"
export PATH="$runtime_bin:$PATH"
exec "$runtime_bin/tsx" "$install_root/gigacode.ts" "$@"
' "$install_root" >"$temporary"
chmod 0755 "$temporary"
mv -f "$temporary" "$destination"
trap - EXIT
ln -sfn "$(basename "$destination")" "$alias_destination"
if [[ -L "$legacy_alias_destination" && "$(readlink "$legacy_alias_destination")" == "$(basename "$destination")" ]]; then
	rm "$legacy_alias_destination"
fi

echo "Installed Gigacode at $destination"
echo "Installed giga alias at $alias_destination"
echo "Installed runtime at $install_root"
if [[ ":$PATH:" != *":$install_dir:"* ]]; then
	echo "Warning: $install_dir is not on PATH" >&2
	exit 1
fi
