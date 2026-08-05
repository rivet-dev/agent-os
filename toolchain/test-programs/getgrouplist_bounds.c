#define _GNU_SOURCE

#include <errno.h>
#include <grp.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define GROUP_CAP 256
#define CANARY UINT64_C(0x51a7cafe93d40b62)

static int group_member_mode(void) {
    errno = 0;
    struct group *group = getgrnam("membercap");
    int saved_errno = errno;
    size_t count = 0;
    if (group != NULL) {
        while (group->gr_mem[count] != NULL) count++;
    }
    printf("group_found=%s\n", group != NULL ? "yes" : "no");
    printf("group_members=%zu\n", count);
    printf("group_overflow=%s\n", saved_errno == EOVERFLOW ? "yes" : "no");
    return 0;
}

static int grouplist_mode(void) {
    struct {
        gid_t groups[GROUP_CAP];
        uint64_t canary;
    } output;
    memset(&output, 0xa5, sizeof output);
    output.canary = CANARY;
    int count = GROUP_CAP;
    errno = 0;
    int result = getgrouplist("boundsuser", 1000, output.groups, &count);
    int saved_errno = errno;

    printf("grouplist_result=%d\n", result);
    printf("grouplist_count=%d\n", count);
    printf("grouplist_overflow=%s\n", saved_errno == EOVERFLOW ? "yes" : "no");
    printf("grouplist_canary=%s\n", output.canary == CANARY ? "yes" : "no");
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fputs("usage: getgrouplist_bounds group-members|grouplist\n", stderr);
        return 2;
    }
    if (strcmp(argv[1], "group-members") == 0) return group_member_mode();
    if (strcmp(argv[1], "grouplist") == 0) return grouplist_mode();
    fprintf(stderr, "unknown mode: %s\n", argv[1]);
    return 2;
}
