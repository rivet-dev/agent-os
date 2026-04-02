# curl upstream build notes

## Goal

Replace the hand-maintained curl WASM build with an upstream release-tarball
pipeline that uses `configure && make`, while keeping local curl changes to a
small carry patch set and pushing the rest of the work into the patched wasi-libc
sysroot and agentOS POSIX runtime.

## Source references

- WAPM/Wasix curl build script:
  - `https://github.com/wapm-packages/curl/blob/master/build.sh`
- Upstream curl install/build docs:
  - `https://github.com/curl/curl/blob/master/docs/INSTALL.md`
  - `https://github.com/curl/curl/blob/master/GIT-INFO.md`
- Upstream tool source list:
  - `https://github.com/curl/curl/blob/master/src/Makefile.inc`
- Browser-oriented wasm notes that are not the right runtime model for agentOS:
  - `https://curl.se/mail/lib-2025-07/0025.html`
  - `https://jeroen.github.io/notes/webassembly-curl/`
  - `https://libcurl.js.org/`

## Key findings

1. Upstream curl does not appear to have a dedicated WASI build system in-tree.
   The normal upstream build remains the autotools/CMake flow from the release
   tarball or generated configure scripts.

2. The Wasix/WAPM approach is effectively:
   - fetch upstream release tarball
   - run `configure` in a WASI/Wasix cross environment
   - run `make`
   - copy out the built `curl` binary

3. The agentOS runtime model is closer to Wasix than to browser wasm ports:
   - sockets are provided by the patched wasi-libc sysroot via `host_net`
   - file/stdio streams are normal WASI file descriptors
   - TLS is a runtime socket-upgrade operation via `host_net.net_tls_connect`

4. The upstream curl 8.11.1 release tarball `configure` already works against
   the patched agentOS sysroot with:
   - `--host=wasm32-unknown-wasi`
   - `--disable-threaded-resolver`

5. Upstream `configure` auto-detects more POSIX and curl features than the
   local handwritten `curl_config.h`, including:
   - cookies
   - proxies
   - bindlocal
   - progress meter
   - IPv6
   - Unix sockets
   - MIME/form API
   - netrc

6. The minimal carry patch set still needed for agentOS is small:
   - `lib/vtls/wasi_tls.c`
   - `lib/vtls/wasi_tls.h`
   - `lib/vtls/vtls.c` registration/include changes
   - `lib/curl_setup.h` so `USE_WASI_TLS` implies `USE_SSL`
   - `lib/hostip.h` guard around `<setjmp.h>`

7. The first upstream compile blocker is not curl-specific logic, but platform
   header behavior:
   - wasi-sdk's `setjmp.h` hard-errors without wasm exception handling
   - upstream `hostip.h` includes `<setjmp.h>` unconditionally
   - our current fork already carries the tiny `__wasi__` guard needed there

8. The platform already appears sufficient for curl's wakeup path without
   implementing POSIX `socketpair()`:
   - upstream `socketpair.c` can fall back to `pipe()`/loopback sockets
   - `configure` found `pipe()`

## Implementation direction

1. Build curl from the official release tarball matching the libcurl version in
   use, instead of manually stitching together `lib/` and `src/`.
2. Apply a tiny overlay patch set for the `wasi-tls` backend and the `setjmp`
   guard.
3. Run upstream `configure` to generate `lib/curl_config.h`.
4. Prefer platform/runtime fixes over curl feature disables wherever possible.

## Likely next platform tasks after the pipeline lands

- Provide `getpwuid_r()` in the patched sysroot if we want better upstream
  compatibility around home-dir lookups and passwd APIs.
- Consider `socketpair()` for broader POSIX completeness, though curl itself can
  operate without it.
- Add `sendmsg()`/`recvmsg()` only if future HTTP/3/QUIC support becomes a goal.
