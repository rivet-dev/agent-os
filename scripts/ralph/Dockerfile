FROM ghcr.io/openai/codex-universal:latest

ENV CARGO_HOME=/workspace/.cargo \
    RUSTUP_HOME=/workspace/.rustup \
    RUSTC=/workspace/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc \
    RUSTDOC=/workspace/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustdoc \
    PATH=/workspace/.cargo/bin:/workspace/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:${PATH}

RUN bash -lc '\
  set -euo pipefail && \
  npm install -g @openai/codex && \
  node_root="$(dirname "$(dirname "$(command -v node)")")" && \
  rm -rf /opt/codex-node && \
  mkdir -p /opt/codex-node && \
  cp -a "${node_root}/." /opt/codex-node/ && \
  chmod -R a+rX /opt/codex-node && \
  for tool in node npm npx pnpm yarn corepack codex; do \
    ln -sf "/opt/codex-node/bin/${tool}" "/usr/local/bin/${tool}"; \
  done'

WORKDIR /workspace
