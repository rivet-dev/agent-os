#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <netdb.h>
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <netinet/in.h>

int main(int argc, char *argv[]) {
    const char *host = argc > 1 ? argv[1] : "httpbin.org";
    const char *port = argc > 2 ? argv[2] : "80";
    const char *path = argc > 3 ? argv[3] : "/get";

    struct addrinfo hints = {0};
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *res;
    int err = getaddrinfo(host, port, &hints, &res);
    if (err != 0) { fprintf(stderr, "getaddrinfo: %s\n", gai_strerror(err)); return 1; }

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) { fprintf(stderr, "socket: %s\n", strerror(errno)); return 1; }
    fprintf(stderr, "socket fd=%d\n", fd);

    /* Set non-blocking like curl does */
    int flags = fcntl(fd, F_GETFL, 0);
    fprintf(stderr, "fcntl F_GETFL=%d\n", flags);
    int r = fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    fprintf(stderr, "fcntl F_SETFL(NONBLOCK)=%d errno=%d\n", r, errno);

    /* TCP_NODELAY */
    int nodelay = 1;
    setsockopt(fd, 6/*IPPROTO_TCP*/, 1/*TCP_NODELAY*/, &nodelay, sizeof(nodelay));

    /* Connect */
    int cr = connect(fd, res->ai_addr, res->ai_addrlen);
    fprintf(stderr, "connect=%d errno=%d\n", cr, errno);
    freeaddrinfo(res);

    /* getsockopt SO_ERROR */
    int so_err = -1;
    socklen_t so_len = sizeof(so_err);
    int gso = getsockopt(fd, SOL_SOCKET, 4/*SO_ERROR*/, &so_err, &so_len);
    fprintf(stderr, "getsockopt(SO_ERROR)=%d value=%d\n", gso, so_err);

    /* getpeername */
    struct sockaddr_in peer = {0};
    socklen_t peerlen = sizeof(peer);
    int gpn = getpeername(fd, (struct sockaddr*)&peer, &peerlen);
    fprintf(stderr, "getpeername=%d errno=%d family=%d\n", gpn, errno, peer.sin_family);

    /* Poll for writability */
    struct pollfd pfd = { .fd = fd, .events = POLLOUT };
    int pr = poll(&pfd, 1, 5000);
    fprintf(stderr, "poll(POLLOUT)=%d revents=0x%x\n", pr, pfd.revents);

    /* Send HTTP request */
    char req[512];
    int reqlen = snprintf(req, sizeof(req),
        "GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n", path, host);
    ssize_t sent = send(fd, req, reqlen, 0);
    fprintf(stderr, "send=%zd\n", sent);

    /* Receive response */
    pfd.events = POLLIN;
    poll(&pfd, 1, 10000);
    char buf[4096];
    ssize_t recvd = recv(fd, buf, sizeof(buf) - 1, 0);
    fprintf(stderr, "recv=%zd\n", recvd);
    if (recvd > 0) { buf[recvd] = '\0'; printf("%s", buf); }

    close(fd);
    return 0;
}
