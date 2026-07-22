#ifndef AGENTOS_XFSTESTS_XFS_XFS_H
#define AGENTOS_XFSTESTS_XFS_XFS_H

#include <errno.h>
#include <linux/limits.h>
#include <linux/types.h>
#include <sys/types.h>

struct xfs_fsop_geom {
    uint64_t datablocks;
    uint64_t rtblocks;
    uint32_t blocksize;
    uint32_t rtextsize;
};

typedef struct {
    int errtag;
    int fd;
} xfs_error_injection_t;

struct xfs_bstat {
    uint64_t bs_ino;
};

struct xfs_fsop_bulkreq {
    __u64 *lastip;
    int icount;
    void *ubuffer;
    int *ocount;
};

struct fsxattr {
    uint32_t fsx_xflags;
    uint32_t fsx_extsize;
    uint32_t fsx_projid;
};

struct dioattr {
    int d_mem;
    int d_miniosz;
    int d_maxiosz;
};

struct xfs_flock64 {
    int16_t l_type;
    int16_t l_whence;
    int64_t l_start;
    int64_t l_len;
    int32_t l_sysid;
    uint32_t l_pid;
};

#define XFS_XFLAG_REALTIME 0x00000001
#define XFS_XFLAG_EXTSIZE 0x00000800

#define XFS_IOC_FSGEOMETRY 1
#define XFS_IOC_ERROR_INJECTION 2
#define XFS_IOC_ERROR_CLEARALL 3
#define XFS_IOC_DIOINFO 4
#define XFS_IOC_FSBULKSTAT 5
#define XFS_IOC_FSBULKSTAT_SINGLE 6
#define XFS_IOC_FSGETXATTR 7
#define XFS_IOC_FSSETXATTR 8
#define XFS_IOC_RESVSP64 9
#define XFS_IOC_UNRESVSP64 10

static inline int xfsctl(const char *path, int fd, int command, void *argument) {
    (void)path;
    (void)fd;
    (void)command;
    (void)argument;
    errno = ENOTTY;
    return -1;
}

#endif
