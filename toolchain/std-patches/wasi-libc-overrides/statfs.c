#include <string.h>
#include <sys/statfs.h>
#include <sys/statvfs.h>

static void from_statvfs(struct statfs *out, const struct statvfs *in) {
    memset(out, 0, sizeof(*out));
    out->f_bsize = in->f_bsize;
    out->f_blocks = in->f_blocks;
    out->f_bfree = in->f_bfree;
    out->f_bavail = in->f_bavail;
    out->f_files = in->f_files;
    out->f_ffree = in->f_ffree;
    out->f_namelen = in->f_namemax;
    out->f_frsize = in->f_frsize;
    out->f_flags = in->f_flag;
}

int statfs(const char *path, struct statfs *out) {
    struct statvfs stat;
    if (statvfs(path, &stat) != 0) return -1;
    from_statvfs(out, &stat);
    return 0;
}

int fstatfs(int fd, struct statfs *out) {
    struct statvfs stat;
    if (fstatvfs(fd, &stat) != 0) return -1;
    from_statvfs(out, &stat);
    return 0;
}
