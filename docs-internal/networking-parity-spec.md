# Networking Parity Spec — curl / wget / git full TLS + network support

Status: implemented · Owner: registry/runtime · Last updated: 2026-07-14

This document preserves the design baseline and decisions that drove the
networking stack. Sections describing missing TLS or future work refer to the
pre-implementation state. For the shipped architecture, see
[`TLS & SSL`](../website/src/content/docs/docs/architecture/tls-ssl.mdx).

## Goal

`curl`, `wget`, and `git` must have **full networking with real TLS/SSL, using
native Linux semantics** — not host-brokered shims. Concretely:

- `curl https://…` and `wget https://…` verify certificates against a real CA
  bundle, honor `--cacert`/`--ca-certificate`, fail with the **real exit codes**
  (curl exit 60 = cert verify failure), and support `--compressed` (gzip, brotli,
  zstd).
- `git clone/fetch/push` over **HTTPS smart-HTTP** works against GitHub/GitLab.
- Certificate trust comes from a **CA bundle inside the VM** (`/etc/ssl/certs/…`),
  the way Debian's `ca-certificates` package + apt/curl/openssl/python all resolve
  it — not from the host machine's trust store.

## Baseline before implementation (verified)

Sockets are **already real** and are not the problem. The patched wasi-libc
sysroot implements `socket()/connect()/getaddrinfo()/send()/recv()` over
`host_net` WASM imports (`toolchain/std-patches/wasi-libc/0008-sockets.patch`,
`0023-host-net-read-write-sockets.patch`; Rust mirror `toolchain/crates/wasi-ext`).
The runner forwards them to the sidecar socket table (`crates/v8-runtime/assets/
runners/wasm-runner.mjs`). So curl/wget/git already do their own DNS, TCP and HTTP
byte-for-byte. **Only TLS and decompression are shimmed or missing.**

| Tool | Networking today | TLS today | Gap |
|---|---|---|---|
| **curl** | real (8.11.1) | host-brokered `wasi_tls.c` → `net_tls_connect` → sidecar rustls + **host** cert store | no real verify, `--cacert`/exit-60/`-v` chain all wrong; no gzip/brotli/zstd |
| **wget** | real (1.24.5) | **none** (`--without-ssl`) — HTTP only | no HTTPS at all; no gzip |
| **git** | real, local-only (2.55.0, `NO_CURL`) | **none** | `git-remote-https` is a dead symlink→git binary; no HTTPS clone/fetch/push |

The current TLS shim (`software/curl/native/c/overlay/lib/vtls/wasi_tls.c`) does
**no handshake and no verification in-guest**: it hands the TCP fd to the host
(`net_tls_connect(fd, host, flags)`), which terminates TLS with rustls +
`rustls_native_certs::load_native_certs()` — **the host machine's** trust store.
`--cacert`/`--capath`/`CURL_CA_BUNDLE`/`SSL_CERT_FILE` are silently ignored; any
TLS error returns curl exit 35 instead of 60; `curl -v` cannot print the chain.

## The decision: in-guest TLS (mbedTLS) + a VM CA bundle

Two directions were researched. **We choose in-guest TLS** because it is the only
one that delivers the acceptance criterion (native Linux semantics + a CA bundle
that mimics apt).

### ✅ Chosen — in-guest mbedTLS 3.6 LTS + `/etc/ssl/certs/ca-certificates.crt`

- **mbedTLS** is pure portable C99, zero platform deps, TLS 1.3, and curl has a
  first-class maintained `USE_MBEDTLS` backend in 8.11.1. Entropy = `getentropy()`
  (already proven in `wasi_tls.c`); clocks/time work; build single-threaded.
- Verification happens **in curl's own code path**: `--cacert` →
  `mbedtls_x509_crt_parse_file` via the VFS, `-k` → generic `verifypeer`, verify
  failure → `CURLE_PEER_FAILED_VERIFICATION` = **exit 60** with the real message,
  `curl -v` prints version/cipher/cert. Exactly Linux curl-with-mbedTLS.
- The sidecar becomes a **dumb ciphertext pipe** — strictly better for the trust
  model (the untrusted executor no longer asks the trusted host to authenticate
  servers on its behalf), and hermetic (no dependence on the host's cert store).

### ❌ Rejected for parity — host-brokered TLS (the current `net_tls_connect` path)

Reusing the existing bridge is minimal work (wget = ~100-line shim, git = build
with the overlaid libcurl), but it **cannot reach native semantics**: it uses the
*host's* cert store (non-hermetic), ignores `--cacert`/`--capath`, returns the
wrong exit-code taxonomy, can't print the cert chain, and every future TLS flag
(client certs, `--pinnedpubkey`, TLS-version pinning) needs a new bridge hop — a
permanent shim treadmill. It also contradicts the "real tool, patch the sysroot"
philosophy the rest of the toolchain follows. Keep the host `net.socket_upgrade_tls`
path only for the **Node/JS runtime**, which is a separate surface.

> Note: the current sidecar TLS path uses `rustls_native_certs` = the **host
> machine's** trust store (`crates/native-sidecar/src/execution.rs`). That is a
> latent hermeticity bug even for the JS runtime — it should read the VM's
> `/etc/ssl` bundle instead. Tracked here; fix alongside.

## CA bundle — ship Debian-shaped trust inside the VM

- Ship the **Mozilla CA bundle** (`curl.se/ca/cacert.pem`, i.e. the set Debian's
  `ca-certificates` produces) at **`/etc/ssl/certs/ca-certificates.crt`**, with the
  conventional `/etc/ssl/cert.pem` symlink. A `ca-certificates` registry package
  owns the payload; VM bootstrap links it into the standard tree (the bootstrap
  already seeds `/etc` in the shadow root — `crates/native-sidecar/src/vm.rs`).
- This one file at that one path is what makes the **whole class** of TLS tools
  "just work": curl's compile-time `CURL_CA_BUNDLE` default, OpenSSL's `OPENSSLDIR`
  (`/usr/lib/ssl` → `/etc/ssl/certs`), apt, python, wget — all resolve there on
  Debian. `SSL_CERT_FILE`/`SSL_CERT_DIR`/`--cacert` env overrides then work for
  free once the backend does real verification.

## Per-tool implementation plan (completed)

### curl
1. Vendor + build **mbedTLS 3.6** for `wasm32-wasip1` in `toolchain/c/Makefile`
   (same pattern as the existing zlib target).
2. In `toolchain/c/scripts/build-curl-upstream.sh`: drop `--without-ssl` and the
   `USE_WASI_TLS` injection; add `--with-mbedtls`, `--with-zlib`, `--with-brotli`,
   `--with-zstd`, `--with-ca-bundle=/etc/ssl/certs/ca-certificates.crt`; retire the
   `wasi_tls.c/h` + `vtls.c` overlay.
3. Tests: assert **exit 60** + real verify message on self-signed without `-k`;
   `--cacert` acceptance of a test CA; `--compressed` gzip/br/zstd round-trips.

### wget
GNU Wget has no mbedTLS backend, so give it a real TLS backend against the same
mbedTLS + CA bundle (its SSL abstraction is just 4 functions in `src/ssl.h`:
`ssl_init`, `ssl_cleanup`, `ssl_connect_wget`, `ssl_check_certificate`).
1. Add `wasi_ssl.c` implementing those 4 functions over **mbedTLS** (handshake +
   `mbedtls_x509_crt_parse_file` on `/etc/ssl/certs/ca-certificates.crt`;
   `--no-check-certificate`/`--ca-certificate`/`--ca-directory` map to real
   mbedTLS verify config, matching Linux wget). Overlay dir mirrors curl's
   (`software/wget/native/c/overlay/…`).
2. In `build-wget-upstream.sh`: keep `./configure --without-ssl` (so it doesn't
   probe GnuTLS/OpenSSL), then post-configure patch `src/wget.h`'s `HAVE_SSL`
   condition to add `|| defined HAVE_WASI_TLS`, pass `-DHAVE_WASI_TLS`, copy the
   overlay, append a `wasi_ssl.o` compile/link rule to the generated `src/Makefile`
   (the exact playbook the curl script uses). This lights up the `https` scheme,
   all `--secure-protocol/--ca-certificate/--no-check-certificate` options, and
   HSTS.
3. Enable **zlib** (`--with-zlib` via `ZLIB_CFLAGS/ZLIB_LIBS` at the existing
   `build/zlib/libz.a`). Leave `--without-libpsl` (cosmetic — cookie public-suffix
   only; wget's built-in heuristic covers single-URL HTTPS).
4. Tests: mirror curl's HTTPS cases (verify-fail on self-signed,
   `--no-check-certificate` success, keep-alive), + gzip via `--compression`.

### git — HTTPS smart-HTTP (clone/fetch/**push**)
Git's HTTP transport lives entirely in the **`git-remote-https` remote helper**,
which links **libcurl in-process** (git never shells out to a `curl` binary). The
current symlink `git-remote-https → git` is structurally broken (`git`'s
`cmd_main` dies "cannot handle remote-https as a builtin").
1. **Produce a reusable libcurl** artifact (`make -C lib install` the overlaid,
   mbedTLS-linked libcurl into a prefix under `toolchain/c/build/curl-upstream/
   install`).
2. In `toolchain/c/scripts/build-git-upstream.sh`: drop `NO_CURL=1`; add
   `CURL_CFLAGS/CURL_LDFLAGS` pointing at that libcurl (defining them skips
   `curl-config`); keep `NO_EXPAT=1`; build targets **`git git-remote-http`**;
   wasm-opt both. (Expect a handful of small WASI compile fixes in
   `http.c`/`remote-curl.c`, same class the curl script already makes; the
   existing `git_compat.c` shims cover most.)
3. **Packaging:** install `git-remote-http` as a **real second command** (replace
   the wrong symlink loop in `toolchain/c/Makefile`); in
   `software/git/agentos-package.json` move `git-remote-http` into `commands` and
   re-point `git-remote-https` → `git-remote-http` (keep `git-upload-pack`/
   `git-receive-pack`/`git-upload-archive` → `git`; those *are* builtins).
4. **Push** is the same libcurl POST plumbing as fetch (`remote-curl.c` spawns
   `git send-pack` and streams the pack via chunked `Transfer-Encoding` +
   `Expect: 100-continue` — all inside libcurl, no new host surface). HTTP Basic
   auth (token/`user:pass` URLs, `GIT_ASKPASS`) works. (`credential-cache`
   unavailable — `NO_UNIX_SOCKETS`; NTLM/Negotiate unsupported — irrelevant for
   GitHub/GitLab.)
5. **Nothing new on the host side** — TLS/DNS/TCP/spawn/permission-tiers are all
   in place. Spawn already resolves `git-remote-https` by name on the guest PATH
   via the `proc_spawn` broker (proven: `git clone` already spawns
   `git upload-pack` locally). Permission tiers already grant `git-remote-http(s)`
   tier `full`.
6. **ssh transport (secondary):** `git@host:` spawns the separately delivered
   in-guest OpenSSH client. SSH uses its own protocol crypto rather than mbedTLS.
7. Tests: flip `hasGitHttpHelper` in `software/git/test/git.test.ts` (the
   smart-HTTP clone/fetch suite is already written but skipped); add a push case
   (small + >1 MiB chunked POST).

## Compression (curl)

- **zlib 1.3.1 is already vendored** and built against this sysroot (git links it):
  `--with-zlib` via `CPPFLAGS/LDFLAGS` at `build/zlib`.
- **brotli** — decoder (`common`+`dec`) is dependency-free portable C; add a
  Makefile fetch/build target mirroring zlib; `--with-brotli` (decode only).
- **zstd** — already compiles under this sysroot inside the duckdb build
  (`toolchain/c/build/duckdb-cmake/third_party/zstd/`); build libzstd
  single-threaded; `--with-zstd`.
- Out of scope here: nghttp2 (HTTP/2), libpsl.

## Refuted / dead-end theses (documented so we don't re-try them)

- **Host-brokered TLS for parity** — works but never reaches native semantics
  (host cert store, ignored `--cacert`, wrong exit codes, no `-v` chain, shim
  treadmill). Rejected as the parity path; kept only for the JS runtime.
- **Full OpenSSL as the shared HTTPS backend** — unnecessary for curl, wget, and
  git. A checksum-pinned OpenSSL 3.5.7 **libcrypto-only** build is proven and is
  now used by OpenSSH to restore RSA/ECDSA/DH/AES algorithm coverage. It builds
  without source patches by disabling threads, assembly, dynamic modules,
  engines, applications, and libssl, and seeds through the owned `getrandom`
  path. That scoped success does not justify replacing the smaller mbedTLS HTTPS
  integrations with the full OpenSSL TLS/provider/config surface.
- **GnuTLS in-guest** (wget's default) — drags nettle → GMP (asm) → libtasn1 →
  p11-kit (dlopen). Non-starter on WASI.
- **rustls + aws-lc-rs in guest** — aws-lc-rs is C+asm, does **not** build for
  wasm32-wasip1. `ring` support is unofficial/spotty. Mixing a Rust staticlib into
  the clang-LTO curl link is fiddly. No fidelity gain over mbedTLS.
- **rustls-native-certs (as the trust story)** — needs an OS cert store; the guest
  has none, and host-side it depends on the host machine's store (non-hermetic).
- **BearSSL** — curl **removed** BearSSL support in 8.12.0 and it lacks TLS 1.3.
  Dead end for any curl upgrade.
- **Reusing curl's `wasi_tls.c` for wget** — impossible; it's written against
  libcurl-internal `Curl_cfilter` APIs. The shareable unit is the **host import
  contract / the mbedTLS backend**, not the C file.
- **git: symlink `git-remote-https` → git binary** (current state) — git's
  `cmd_main` hard-dies for a non-builtin `git-*` argv[0]. Structurally impossible.
- **git: spawn the `curl` CLI from a shell helper** — the remote helper is a
  long-lived, stateful, bidirectional protocol-v2 program (auth retry, gzip,
  full-duplex chunked POST). The CLI can't provide it.
- **git: gitoxide / libgit2 / reimplement smart protocol in-process** — not real
  upstream git; diverges on CLI/config/hooks; would need its own WASI net port
  anyway. Rejected.
- **git: dumb-HTTP only** — GitHub/GitLab/Gitea serve smart-HTTP only for fetch
  and never support dumb push. Non-starter (dumb fetch rides along free).
- **Anything threaded** — the runner is single-threaded wasip1; all TLS/compression
  libs build single-threaded (mbedTLS/zstd support it).

## Work breakdown & sequencing

1. **mbedTLS 3.6** target in `toolchain/c/Makefile` (shared by curl, wget, git's
   libcurl). ← foundational, do first.
2. **CA bundle**: `ca-certificates` payload at `/etc/ssl/certs/ca-certificates.crt`
   + VM bootstrap seeding + sidecar reads the VM bundle (not the host store). ←
   foundational.
3. **curl**: mbedTLS + zlib/brotli/zstd + CA bundle; retire `wasi_tls.c`; tests.
4. **libcurl install artifact** (from step 3's build) for git to link.
5. **git**: build-with-curl, real `git-remote-http` helper, packaging, push; tests.
6. **wget**: `wasi_ssl.c` mbedTLS backend + zlib + CA bundle; tests.

Steps 3/5/6 all sit on 1+2. One jj rev per tool. Real e2e tests against a TLS
server (verify-fail on self-signed, `--cacert` success, real clone/fetch/push) are
the deliverable — no host-store shortcuts.
