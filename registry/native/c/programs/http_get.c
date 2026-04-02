/* http_get.c — connect to an HTTP server, send GET request, print response body */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>

int main(int argc, char *argv[]) {
    if (argc < 2) {
        fprintf(stderr, "usage: http_get <port> [path] [output_file]\n");
        return 1;
    }

    int port = atoi(argv[1]);
    const char *path = argc >= 3 ? argv[2] : "/";
    const char *output_file = argc >= 4 ? argv[3] : NULL;

    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("socket");
        return 1;
    }

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);

    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("connect");
        close(fd);
        return 1;
    }

    char request[1024];
    int request_len = snprintf(
        request,
        sizeof(request),
        "GET %s HTTP/1.0\r\nHost: localhost\r\n\r\n",
        path
    );
    if (request_len < 0 || (size_t)request_len >= sizeof(request)) {
        fprintf(stderr, "request path too long\n");
        close(fd);
        return 1;
    }

    ssize_t sent = send(fd, request, (size_t)request_len, 0);
    if (sent < 0) {
        perror("send");
        close(fd);
        return 1;
    }

    /* Read full response */
    char response[4096];
    size_t total = 0;
    ssize_t n;
    while ((n = recv(fd, response + total, sizeof(response) - total - 1, 0)) > 0) {
        total += (size_t)n;
    }
    response[total] = '\0';

    close(fd);

    /* Find body after \r\n\r\n */
    const char *body = strstr(response, "\r\n\r\n");
    if (body) {
        body += 4;
        if (output_file) {
            FILE *out = fopen(output_file, "wb");
            if (!out) {
                perror("fopen");
                return 1;
            }
            size_t body_len = total - (size_t)(body - response);
            if (fwrite(body, 1, body_len, out) != body_len) {
                perror("fwrite");
                fclose(out);
                return 1;
            }
            fclose(out);
        } else {
            printf("body: %s\n", body);
        }
    } else {
        printf("body: (no separator found)\n");
        return 1;
    }

    return 0;
}
