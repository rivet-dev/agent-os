/**
 * getnameinfo() override.
 *
 * The patched WASI sockets layer in 0008-sockets.patch provides
 * getaddrinfo() via host_net imports but does not implement the reverse
 * direction. DuckDB v1.5+ pulls in src/main/http/ which links against
 * getnameinfo, so the binary fails to link without this stub.
 *
 * We don't have a host_net.net_getnameinfo import — DNS reverse lookups
 * are out of scope for now. This stub formats sockaddr_in / sockaddr_in6
 * numerically (NI_NUMERICHOST behavior) and writes the port to serv.
 * Programs that explicitly request name resolution get EAI_FAIL.
 */

#include <netdb.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <arpa/inet.h>
#include <string.h>
#include <stdio.h>

int getnameinfo(const struct sockaddr *restrict sa, socklen_t salen,
                char *restrict host, socklen_t hostlen,
                char *restrict serv, socklen_t servlen,
                int flags)
{
    (void)flags;

    if (host && hostlen > 0) {
        const void *src = NULL;
        if (sa->sa_family == AF_INET && salen >= sizeof(struct sockaddr_in)) {
            src = &((const struct sockaddr_in *)sa)->sin_addr;
            if (!inet_ntop(AF_INET, src, host, hostlen)) return EAI_OVERFLOW;
        } else if (sa->sa_family == AF_INET6 && salen >= sizeof(struct sockaddr_in6)) {
            src = &((const struct sockaddr_in6 *)sa)->sin6_addr;
            if (!inet_ntop(AF_INET6, src, host, hostlen)) return EAI_OVERFLOW;
        } else {
            return EAI_FAMILY;
        }
    }

    if (serv && servlen > 0) {
        unsigned short port = 0;
        if (sa->sa_family == AF_INET) {
            port = ntohs(((const struct sockaddr_in *)sa)->sin_port);
        } else if (sa->sa_family == AF_INET6) {
            port = ntohs(((const struct sockaddr_in6 *)sa)->sin6_port);
        }
        int n = snprintf(serv, servlen, "%u", (unsigned)port);
        if (n < 0 || (socklen_t)n >= servlen) return EAI_OVERFLOW;
    }

    return 0;
}
