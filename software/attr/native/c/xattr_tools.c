#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <regex.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

static const char b64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

static const char *base_name(const char *path) {
    const char *slash = strrchr(path, '/');
    return slash ? slash + 1 : path;
}

static const char *xattr_error(void) {
    return errno == ENODATA ? "No such attribute" : strerror(errno);
}

static void attr_error(const char *operation, const char *name, const char *path) {
    int saved_errno = errno;
    fprintf(stderr, "attr_%s: %s\n", operation, strerror(saved_errno));
    fprintf(stderr, "Could not %s \"%s\" for %s\n", operation, name, path);
}

static int hex_digit(int c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static unsigned char *decode_value(const char *text, size_t *length) {
    if (!text) {
        *length = 0;
        return calloc(1, 1);
    }
    if (!strncmp(text, "0x", 2)) {
        size_t digits = strlen(text + 2);
        if (digits % 2) return NULL;
        unsigned char *out = malloc(digits / 2 + 1);
        if (!out) return NULL;
        for (size_t i = 0; i < digits; i += 2) {
            int high = hex_digit(text[i + 2]);
            int low = hex_digit(text[i + 3]);
            if (high < 0 || low < 0) { free(out); return NULL; }
            out[i / 2] = (unsigned char)((high << 4) | low);
        }
        *length = digits / 2;
        return out;
    }
    if (!strncmp(text, "0s", 2)) {
        const char *input = text + 2;
        size_t input_len = strlen(input);
        unsigned char *out = malloc(input_len * 3 / 4 + 3);
        if (!out) return NULL;
        size_t used = 0;
        unsigned value = 0;
        int bits = 0;
        for (size_t i = 0; i < input_len && input[i] != '='; i++) {
            const char *found = strchr(b64, input[i]);
            if (!found) { free(out); return NULL; }
            value = (value << 6) | (unsigned)(found - b64);
            bits += 6;
            if (bits >= 8) {
                bits -= 8;
                out[used++] = (unsigned char)(value >> bits);
                value &= (1u << bits) - 1;
            }
        }
        *length = used;
        return out;
    }
    size_t text_len = strlen(text);
    if (text_len >= 2 && text[0] == '"' && text[text_len - 1] == '"') {
        text++;
        text_len -= 2;
        unsigned char *out = malloc(text_len + 1);
        if (!out) return NULL;
        size_t used = 0;
        for (size_t i = 0; i < text_len; i++) {
            if (text[i] == '\\' && i + 1 < text_len) {
                if (i + 3 < text_len && text[i + 1] >= '0' && text[i + 1] <= '7' &&
                    text[i + 2] >= '0' && text[i + 2] <= '7' &&
                    text[i + 3] >= '0' && text[i + 3] <= '7') {
                    out[used++] = (unsigned char)(((text[i + 1] - '0') << 6) |
                                                  ((text[i + 2] - '0') << 3) |
                                                  (text[i + 3] - '0'));
                    i += 3;
                    continue;
                }
                i++;
            }
            out[used++] = (unsigned char)text[i];
        }
        *length = used;
        return out;
    }
    unsigned char *out = malloc(text_len + 1);
    if (!out) return NULL;
    memcpy(out, text, text_len);
    *length = text_len;
    return out;
}

static void print_encoded(const unsigned char *value, size_t length, const char *encoding) {
    if (!strcmp(encoding, "hex")) {
        fputs("0x", stdout);
        for (size_t i = 0; i < length; i++) printf("%02x", value[i]);
        return;
    }
    if (!strcmp(encoding, "base64")) {
        fputs("0s", stdout);
        for (size_t i = 0; i < length; i += 3) {
            unsigned word = (unsigned)value[i] << 16;
            int count = (int)(length - i);
            if (count > 1) word |= (unsigned)value[i + 1] << 8;
            if (count > 2) word |= value[i + 2];
            putchar(b64[(word >> 18) & 63]);
            putchar(b64[(word >> 12) & 63]);
            putchar(count > 1 ? b64[(word >> 6) & 63] : '=');
            putchar(count > 2 ? b64[word & 63] : '=');
        }
        return;
    }
    putchar('"');
    for (size_t i = 0; i < length; i++) {
        unsigned char c = value[i];
        if (c == '\\' || c == '"') putchar('\\');
        if (isprint(c)) putchar(c);
        else printf("\\%03o", c);
    }
    putchar('"');
}

static ssize_t get_value(const char *path, const char *name, int nofollow,
                         unsigned char **value) {
    ssize_t size = nofollow ? lgetxattr(path, name, NULL, 0)
                            : getxattr(path, name, NULL, 0);
    if (size < 0) return -1;

    for (int attempt = 0; attempt < 2; attempt++) {
        size_t capacity = attempt == 0 ? (size_t)size : XATTR_SIZE_MAX;
        *value = malloc(capacity + 1);
        if (!*value) { errno = ENOMEM; return -1; }
        ssize_t result = nofollow ? lgetxattr(path, name, *value, capacity)
                                  : getxattr(path, name, *value, capacity);
        if (result >= 0) return result;

        int saved_errno = errno;
        free(*value);
        *value = NULL;
        if (saved_errno != ERANGE || capacity == XATTR_SIZE_MAX) {
            errno = saved_errno;
            return -1;
        }
    }
    errno = ERANGE;
    return -1;
}

static int list_names(const char *path, int nofollow, char **list, ssize_t *size) {
    *size = nofollow ? llistxattr(path, NULL, 0) : listxattr(path, NULL, 0);
    if (*size < 0) return -1;

    for (int attempt = 0; attempt < 2; attempt++) {
        size_t capacity = attempt == 0 ? (size_t)*size : XATTR_SIZE_MAX;
        *list = malloc(capacity + 1);
        if (!*list) { errno = ENOMEM; return -1; }
        ssize_t result = nofollow ? llistxattr(path, *list, capacity)
                                  : listxattr(path, *list, capacity);
        if (result >= 0) {
            *size = result;
            (*list)[*size] = 0;
            return 0;
        }

        int saved_errno = errno;
        free(*list);
        *list = NULL;
        if (saved_errno != ERANGE || capacity == XATTR_SIZE_MAX) {
            errno = saved_errno;
            return -1;
        }
    }
    errno = ERANGE;
    return -1;
}

struct get_options {
    const char *name;
    const char *match;
    const char *encoding;
    int dump;
    int only_values;
    int absolute;
    int nofollow;
    int recursive;
    int walk_follow;
};

static int get_one(const char *path, const struct get_options *options) {
    char *list = NULL;
    ssize_t list_size = 0;
    regex_t regex;
    int have_regex = options->match && regcomp(&regex, options->match, REG_EXTENDED | REG_NOSUB) == 0;
    if (options->name) {
        list_size = (ssize_t)strlen(options->name) + 1;
        list = strdup(options->name);
    } else if (list_names(path, options->nofollow, &list, &list_size) != 0) {
        fprintf(stderr, "getfattr: %s: %s\n", path, xattr_error());
        if (have_regex) regfree(&regex);
        return 1;
    }

    int printed_header = 0;
    int status = 0;
    for (char *name = list; name < list + list_size; name += strlen(name) + 1) {
        if (!options->name && have_regex && regexec(&regex, name, 0, NULL, 0) != 0) continue;
        unsigned char *value = NULL;
        ssize_t size = get_value(path, name, options->nofollow, &value);
        if (size < 0) {
            if (options->name && errno == ENODATA)
                fprintf(stderr, "%s: %s: %s\n", path, name, xattr_error());
            else
                fprintf(stderr, "getfattr: %s: %s\n", path, xattr_error());
            status = 1;
            continue;
        }
        if (options->only_values) {
            fwrite(value, 1, (size_t)size, stdout);
            free(value);
            continue;
        }
        if (!printed_header) {
            const char *display = path;
            if (!options->absolute) while (*display == '/') display++;
            printf("# file: %s\n", display);
            printed_header = 1;
        }
        printf("%s=", name);
        print_encoded(value, (size_t)size, options->encoding);
        putchar('\n');
        free(value);
    }
    if (printed_header) putchar('\n');
    free(list);
    if (have_regex) regfree(&regex);
    return status;
}

static int walk_get(const char *path, const struct get_options *options) {
    int status = get_one(path, options);
    if (!options->recursive) return status;
    struct stat st;
    int stat_result = options->walk_follow ? stat(path, &st) : lstat(path, &st);
    if (stat_result != 0 || !S_ISDIR(st.st_mode)) return status;
    DIR *dir = opendir(path);
    if (!dir) return 1;
    struct dirent *entry;
    while ((entry = readdir(dir))) {
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, "..")) continue;
        size_t length = strlen(path) + strlen(entry->d_name) + 2;
        char *child = malloc(length);
        if (!child) { status = 1; break; }
        snprintf(child, length, "%s/%s", path, entry->d_name);
        status |= walk_get(child, options);
        free(child);
    }
    closedir(dir);
    return status;
}

static int getfattr_main(int argc, char **argv) {
    struct get_options options = {.match = "^user\\.", .encoding = "text"};
    int first_path = argc;
    for (int i = 1; i < argc; i++) {
        const char *arg = argv[i];
        if ((!strcmp(arg, "-n") || !strcmp(arg, "--name")) && i + 1 < argc) options.name = argv[++i];
        else if (!strncmp(arg, "--name=", 7)) options.name = arg + 7;
        else if ((!strcmp(arg, "-m") || !strcmp(arg, "--match")) && i + 1 < argc) options.match = argv[++i];
        else if (!strncmp(arg, "--match=", 8)) options.match = arg + 8;
        else if ((!strcmp(arg, "-e") || !strcmp(arg, "--encoding")) && i + 1 < argc) options.encoding = argv[++i];
        else if (!strncmp(arg, "--encoding=", 11)) options.encoding = arg + 11;
        else if (!strncmp(arg, "-n", 2) && arg[2]) options.name = arg + 2;
        else if (!strncmp(arg, "-m", 2) && arg[2]) options.match = arg + 2;
        else if (!strncmp(arg, "-e", 2) && arg[2]) options.encoding = arg + 2;
        else if (!strcmp(arg, "-d") || !strcmp(arg, "--dump")) options.dump = 1;
        else if (!strcmp(arg, "--only-values")) options.only_values = 1;
        else if (!strcmp(arg, "--absolute-names")) options.absolute = 1;
        else if (!strcmp(arg, "-h") || !strcmp(arg, "--no-dereference")) options.nofollow = 1;
        else if (!strcmp(arg, "-P")) options.walk_follow = 0;
        else if (!strcmp(arg, "-L")) options.walk_follow = 1;
        else if (!strcmp(arg, "-R") || !strcmp(arg, "--recursive")) options.recursive = 1;
        else if (arg[0] == '-' && arg[1] != '-' && arg[1] != 0) {
            for (const char *flag = arg + 1; *flag; flag++) {
                if (*flag == 'd') options.dump = 1;
                else if (*flag == 'h') options.nofollow = 1;
                else if (*flag == 'P') options.walk_follow = 0;
                else if (*flag == 'L') options.walk_follow = 1;
                else if (*flag == 'R') options.recursive = 1;
            }
        }
        else if (arg[0] == '-') continue;
        else { first_path = i; break; }
    }
    if (first_path == argc) { fprintf(stderr, "getfattr: missing file operand\n"); return 1; }
    int status = 0;
    for (int i = first_path; i < argc; i++) status |= walk_get(argv[i], &options);
    return status;
}

static int apply_set(const char *path, const char *name, const char *text,
                     int remove_attr, int nofollow) {
    if (remove_attr) {
        int result = nofollow ? lremovexattr(path, name) : removexattr(path, name);
        if (result != 0) fprintf(stderr, "setfattr: %s: %s\n", path, xattr_error());
        return result != 0;
    }
    size_t length = 0;
    unsigned char *value = decode_value(text, &length);
    if (!value) { fprintf(stderr, "setfattr: invalid value encoding\n"); return 1; }
    int result = nofollow ? lsetxattr(path, name, value, length, 0)
                          : setxattr(path, name, value, length, 0);
    if (result != 0) fprintf(stderr, "setfattr: %s: %s\n", path, xattr_error());
    free(value);
    return result != 0;
}

static int restore_attrs(const char *filename, int nofollow) {
    FILE *input = !strcmp(filename, "-") ? stdin : fopen(filename, "r");
    if (!input) { perror("setfattr restore"); return 1; }
    const size_t line_capacity = 131072;
    char *line = malloc(line_capacity);
    if (!line) {
        if (input != stdin) fclose(input);
        fprintf(stderr, "setfattr: restore buffer allocation failed\n");
        return 1;
    }
    char path[4096] = {0};
    int status = 0;
    while (fgets(line, line_capacity, input)) {
        line[strcspn(line, "\r\n")] = 0;
        if (!strncmp(line, "# file: ", 8)) {
            snprintf(path, sizeof(path), "%s", line + 8);
        } else if (line[0] && line[0] != '#') {
            char *equals = strchr(line, '=');
            if (equals) { *equals = 0; status |= apply_set(path, line, equals + 1, 0, nofollow); }
        }
    }
    free(line);
    if (input != stdin) fclose(input);
    return status;
}

static int setfattr_main(int argc, char **argv) {
    const char *name = NULL;
    const char *value = NULL;
    const char *restore = NULL;
    int remove_attr = 0;
    int nofollow = 0;
    int first_path = argc;
    for (int i = 1; i < argc; i++) {
        const char *arg = argv[i];
        if ((!strcmp(arg, "-n") || !strcmp(arg, "--name")) && i + 1 < argc) name = argv[++i];
        else if (!strncmp(arg, "--name=", 7)) name = arg + 7;
        else if ((!strcmp(arg, "-v") || !strcmp(arg, "--value")) && i + 1 < argc) value = argv[++i];
        else if (!strncmp(arg, "--value=", 8)) value = arg + 8;
        else if ((!strcmp(arg, "-x") || !strcmp(arg, "--remove")) && i + 1 < argc) { name = argv[++i]; remove_attr = 1; }
        else if (!strncmp(arg, "--remove=", 9)) { name = arg + 9; remove_attr = 1; }
        else if (!strcmp(arg, "-h") || !strcmp(arg, "--no-dereference")) nofollow = 1;
        else if (!strncmp(arg, "--restore=", 10)) restore = arg + 10;
        else if (!strcmp(arg, "--restore") && i + 1 < argc) restore = argv[++i];
        else if (arg[0] == '-') continue;
        else { first_path = i; break; }
    }
    if (restore) return restore_attrs(restore, nofollow);
    if (!name || first_path == argc) { fprintf(stderr, "setfattr: missing attribute or file\n"); return 1; }
    int status = 0;
    for (int i = first_path; i < argc; i++) status |= apply_set(argv[i], name, value, remove_attr, nofollow);
    return status;
}

static char *user_name(const char *name) {
    size_t length = strlen(name) + 6;
    char *full = malloc(length);
    if (full) snprintf(full, length, "user.%s", name);
    return full;
}

static int attr_main(int argc, char **argv) {
    const char *set_name = NULL, *get_name = NULL, *remove_name = NULL, *value_arg = NULL;
    int list = 0, quiet = 0, first_path = argc;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "-s") && i + 1 < argc) set_name = argv[++i];
        else if (!strcmp(argv[i], "-g") && i + 1 < argc) get_name = argv[++i];
        else if (!strcmp(argv[i], "-r") && i + 1 < argc) remove_name = argv[++i];
        else if (!strcmp(argv[i], "-V") && i + 1 < argc) value_arg = argv[++i];
        else if (!strcmp(argv[i], "-l")) list = 1;
        else if (!strcmp(argv[i], "-q")) quiet = 1;
        else if (argv[i][0] != '-') { first_path = i; break; }
    }
    if (first_path == argc) { fprintf(stderr, "attr: missing file operand\n"); return 1; }
    const char *path = argv[first_path];
    if (list) {
        char *names = NULL; ssize_t size = 0;
        if (list_names(path, 0, &names, &size) != 0) { fprintf(stderr, "attr: %s: %s\n", path, xattr_error()); return 1; }
        for (char *name = names; name < names + size; name += strlen(name) + 1) {
            if (strncmp(name, "user.", 5)) continue;
            unsigned char *value = NULL; ssize_t value_size = get_value(path, name, 0, &value);
            if (value_size >= 0) printf("Attribute \"%s\" has a %zd byte value for %s\n", name + 5, value_size, path);
            free(value);
        }
        free(names); return 0;
    }
    const char *short_name = set_name ? set_name : get_name ? get_name : remove_name;
    if (!short_name) return 1;
    char *name = user_name(short_name);
    if (!name) return 1;
    if (remove_name) {
        int result = removexattr(path, name);
        if (result && !quiet) attr_error("remove", short_name, path);
        free(name); return result != 0;
    }
    if (get_name) {
        unsigned char *value = NULL; ssize_t size = get_value(path, name, 0, &value);
        if (size < 0) { if (!quiet) attr_error("get", short_name, path); free(name); return 1; }
        if (!quiet) printf("Attribute \"%s\" had a %zd byte value for %s:\n", short_name, size, path);
        fwrite(value, 1, (size_t)size, stdout);
        if (!quiet) putchar('\n');
        free(value); free(name); return 0;
    }
    unsigned char *value = NULL; size_t size = 0;
    if (value_arg) value = decode_value(value_arg, &size);
    else {
        size_t capacity = 4096; value = malloc(capacity);
        if (!value) { free(name); return 1; }
        int c; while ((c = getchar()) != EOF) { if (size == capacity) { capacity *= 2; value = realloc(value, capacity); if (!value) { free(name); return 1; } } value[size++] = (unsigned char)c; }
    }
    int result = setxattr(path, name, value, size, 0);
    if (result && !quiet) attr_error("set", short_name, path);
    else if (!quiet) {
        printf("Attribute \"%s\" set to a %zu byte value for %s:\n", short_name, size, path);
        fwrite(value, 1, size, stdout);
        putchar('\n');
    }
    free(value); free(name); return result != 0;
}

int main(int argc, char **argv) {
    const char *program = base_name(argv[0]);
    if (!strcmp(program, "getfattr")) return getfattr_main(argc, argv);
    if (!strcmp(program, "setfattr")) return setfattr_main(argc, argv);
    return attr_main(argc, argv);
}
