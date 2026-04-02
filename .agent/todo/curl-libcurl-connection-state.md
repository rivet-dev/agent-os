# curl: libcurl connection state machine fails despite working socket layer

## Status
The WASM curl binary builds and the underlying socket layer (host_socket.c) is fully functional. Raw C programs using socket/connect/poll/send/recv work correctly, including with non-blocking sockets. However, libcurl's internal connection state machine (Happy Eyeballs / eyeballer mechanism) still reports "Failed to connect" despite the kernel successfully establishing TCP connections.

## What works
- Patched wasi-libc sysroot builds with socket support (host_socket.c)
- socket(), connect(), poll(), send(), recv() all work correctly
- Non-blocking sockets via fcntl(F_SETFL, O_NONBLOCK) work
- getsockopt(SO_ERROR) returns 0 (no pending errors)
- getpeername() returns the peer address
- DNS resolution via getaddrinfo() works
- The `http_get_test` C program successfully makes HTTP requests

## What fails
- libcurl's `curl_easy_perform()` always returns CURLE_COULDNT_CONNECT (7)
- curl verbose output shows "Failed to connect to X port Y after 0 ms: Error" with no "Trying" line
- This suggests the failure happens in libcurl's connection filter chain, before the actual connect attempt logging

## Root cause hypothesis
The issue is likely in libcurl's `Curl_conn_cf_connect()` connection filter chain. Possible causes:
1. libcurl's `curlx_nonblock()` calls fcntl which succeeds, but then the connection filter's internal state tracking gets confused
2. The `Curl_conn_is_ip_connected()` check uses some mechanism we haven't implemented
3. libcurl's socket pair (for internal signaling) might fail — `socketpair()` is not implemented

## Files involved
- `/registry/native/c/libs/curl/lib/curl_config.h` - curl build configuration for WASI
- `/registry/native/c/libs/curl/lib/wasi_stubs.c` - WASI-specific stubs (now empty)
- `/registry/native/c/libs/curl/lib/vtls/wasi_tls.c` - TLS backend using host_net
- `/registry/native/c/vendor/wasi-libc/libc-bottom-half/sources/host_socket.c` - socket implementation
- `/registry/native/c/programs/http_get_test.c` - working raw socket test program

## Next steps
1. Add debug prints to libcurl's `connect.c` singleipconnect() to trace exactly where it fails
2. Check if `socketpair()` is needed and not implemented
3. Check if `Curl_conn_cf_get_socket()` returns the right FD
4. Consider patching libcurl to use synchronous blocking mode instead of non-blocking
