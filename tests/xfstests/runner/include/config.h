#ifndef AGENTOS_XFSTESTS_CONFIG_H
#define AGENTOS_XFSTESTS_CONFIG_H

/* Minimal cross-build feature map for xfstests helpers compiled against WASI libc. */
#define STDC_HEADERS 1
#define HAVE_ASSERT_H 1
#define HAVE_DIRENT_H 1
#define HAVE_ERRNO_H 1
#define HAVE_LIBGEN_H 1
#define HAVE_MALLOC_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRING_H 1
#define HAVE_STRINGS_H 1
#define HAVE_SYS_FCNTL_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_STATVFS_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_TYPES_H 1
#define HAVE_TIME_H 1
#define HAVE_UNISTD_H 1
#define HAVE_XFS_XFS_H 1

#endif
