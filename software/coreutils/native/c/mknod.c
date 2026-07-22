#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef __wasm__
__attribute__((import_module("host_fs"), import_name("path_mknod")))
uint32_t agentos_path_mknod(uint32_t fd, const char *path, uint32_t path_len,
                            uint32_t mode, uint64_t rdev);
#endif

#define AT_FDCWD_SENTINEL UINT32_MAX
#define S_IFIFO_AGENTOS 0010000u
#define S_IFCHR_AGENTOS 0020000u
#define S_IFBLK_AGENTOS 0060000u

static int parse_number(const char *text, unsigned long *value) {
    char *end = NULL;
    errno = 0;
    *value = strtoul(text, &end, 0);
    return errno == 0 && end != text && *end == '\0';
}

static const char *base_name(const char *path) {
    const char *slash = strrchr(path, '/');
    return slash ? slash + 1 : path;
}

static int create_node(const char *path, uint32_t type, uint32_t permissions,
                       unsigned long major, unsigned long minor) {
#ifdef __wasm__
    uint64_t rdev = ((uint64_t)(major & 0xfff) << 8) |
                    ((uint64_t)(major & ~0xffful) << 32) |
                    ((uint64_t)minor & 0xff) |
                    ((uint64_t)(minor & ~0xfful) << 12);
    uint32_t error = agentos_path_mknod(AT_FDCWD_SENTINEL, path,
                                        (uint32_t)strlen(path),
                                        type | permissions, rdev);
    if (error != 0) {
        fprintf(stderr, "mknod: %s: %s\n", path, strerror((int)error));
        return 1;
    }
    return 0;
#else
    (void)path;
    (void)type;
    (void)permissions;
    (void)major;
    (void)minor;
    fprintf(stderr, "mknod: agentOS host import is only available in a VM\n");
    return 1;
#endif
}

int main(int argc, char **argv) {
    uint32_t permissions = 0666u;
    int index = 1;
    if (index + 1 < argc && strcmp(argv[index], "-m") == 0) {
        unsigned long parsed_mode;
        if (!parse_number(argv[index + 1], &parsed_mode) || parsed_mode > 07777) {
            fprintf(stderr, "%s: invalid mode\n", base_name(argv[0]));
            return 1;
        }
        permissions = (uint32_t)parsed_mode;
        index += 2;
    }

    if (strcmp(base_name(argv[0]), "mkfifo") == 0) {
        if (index == argc) {
            fprintf(stderr, "usage: mkfifo [-m MODE] NAME...\n");
            return 1;
        }
        for (; index < argc; index++) {
            if (create_node(argv[index], S_IFIFO_AGENTOS, permissions, 0, 0) != 0)
                return 1;
        }
        return 0;
    }

    if (index + 1 >= argc) {
        fprintf(stderr, "usage: mknod [-m MODE] NAME TYPE [MAJOR MINOR]\n");
        return 1;
    }
    const char *path = argv[index++];
    const char *type_name = argv[index++];
    if (strcmp(type_name, "p") == 0) {
        if (index != argc) return 1;
        return create_node(path, S_IFIFO_AGENTOS, permissions, 0, 0);
    }

    uint32_t type;
    if (strcmp(type_name, "c") == 0 || strcmp(type_name, "u") == 0)
        type = S_IFCHR_AGENTOS;
    else if (strcmp(type_name, "b") == 0)
        type = S_IFBLK_AGENTOS;
    else {
        fprintf(stderr, "mknod: unsupported type '%s'\n", type_name);
        return 1;
    }
    unsigned long major;
    unsigned long minor;
    if (index + 2 != argc || !parse_number(argv[index], &major) ||
        !parse_number(argv[index + 1], &minor)) {
        fprintf(stderr, "usage: mknod [-m MODE] NAME TYPE MAJOR MINOR\n");
        return 1;
    }
    return create_node(path, type, permissions, major, minor);
}
