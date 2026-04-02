/* getsockopt shim — routes through host_net.net_getsockopt WASM import.
 *
 * The WASI socket patch provides setsockopt but not getsockopt. curl calls
 * getsockopt(SOL_SOCKET, SO_ERROR) in verifyconnect() to check the result
 * of a non-blocking connect. Without this shim, getsockopt returns -1 and
 * curl treats every connection as failed (exit code 7).
 */

#include <sys/socket.h>
#include <errno.h>
#include <stdint.h>

#define WASM_IMPORT(mod, fn) \
    __attribute__((__import_module__(mod), __import_name__(fn)))

WASM_IMPORT("host_net", "net_getsockopt")
uint32_t __host_net_getsockopt(uint32_t fd, uint32_t level, uint32_t optname,
    uint8_t *optval_ptr, uint32_t *optval_len_ptr);

int getsockopt(int sockfd, int level, int optname, void *restrict optval,
               socklen_t *restrict optlen) {
    if (optval == NULL || optlen == NULL) {
        errno = EINVAL;
        return -1;
    }
    uint32_t len = (uint32_t)*optlen;
    uint32_t err = __host_net_getsockopt(
        (uint32_t)sockfd, (uint32_t)level, (uint32_t)optname,
        (uint8_t *)optval, &len);
    if (err != 0) {
        errno = (int)err;
        return -1;
    }
    *optlen = (socklen_t)len;
    return 0;
}
