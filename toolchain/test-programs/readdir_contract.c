#define _DEFAULT_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#ifdef __wasi__
#include <wasi/api.h>

/* White-box access used to deterministically test fdopendir's lazy-refill
 * sentinel. This is the libc implementation header for the sysroot under
 * test, not a guest-facing API. */
#include "../c/vendor/wasi-libc/libc-bottom-half/cloudlibc/src/libc/dirent/dirent_impl.h"
#endif

#define FILE_COUNT 220
#define MAX_ENTRIES (FILE_COUNT + 4)

struct observed_entry {
	char name[256];
	ino_t ino;
};

static int compare_entry(const void *left, const void *right) {
	const struct observed_entry *a = left;
	const struct observed_entry *b = right;
	return strcmp(a->name, b->name);
}

static int collect(const char *path, struct observed_entry *entries,
    size_t *count, int *dots, int *nonzero, int *matches_stat) {
	DIR *dir = opendir(path);
	struct dirent *entry;

	if (dir == NULL)
		return -1;
	*count = 0;
	*dots = 0;
	*nonzero = 1;
	*matches_stat = 1;
	errno = 0;
	while ((entry = readdir(dir)) != NULL) {
		if (*count >= MAX_ENTRIES) {
			closedir(dir);
			errno = EOVERFLOW;
			return -1;
		}
		if (strcmp(entry->d_name, ".") == 0 ||
		    strcmp(entry->d_name, "..") == 0)
			(*dots)++;
		if (entry->d_ino == 0)
			*nonzero = 0;
		{
			struct stat stat_buffer;
			if (fstatat(dirfd(dir), entry->d_name, &stat_buffer,
			    AT_SYMLINK_NOFOLLOW) != 0 ||
			    stat_buffer.st_ino != entry->d_ino)
				*matches_stat = 0;
		}
		snprintf(entries[*count].name, sizeof(entries[*count].name), "%s",
		    entry->d_name);
		entries[*count].ino = entry->d_ino;
		(*count)++;
	}
	if (errno != 0 || closedir(dir) != 0)
		return -1;
	qsort(entries, *count, sizeof(entries[0]), compare_entry);
	return 0;
}

static int verify_seekdir(const char *path) {
	DIR *dir = opendir(path);
	struct dirent *entry;
	char expected_name[256];
	ino_t expected_ino;
	long cookie = 0;
	int index;

	if (dir == NULL)
		return 0;
	for (index = 0; index < 150; index++) {
		if (readdir(dir) == NULL) {
			closedir(dir);
			return 0;
		}
	}
	cookie = telldir(dir);
	entry = readdir(dir);
	if (entry == NULL) {
		closedir(dir);
		return 0;
	}
	snprintf(expected_name, sizeof(expected_name), "%s", entry->d_name);
	expected_ino = entry->d_ino;
	seekdir(dir, cookie);
	entry = readdir(dir);
	index = entry != NULL && strcmp(entry->d_name, expected_name) == 0 &&
	    entry->d_ino == expected_ino;
	closedir(dir);
	return index;
}

static int verify_all_seekdir_positions(const char *path) {
	struct observed_entry entries[MAX_ENTRIES];
	long cookies[MAX_ENTRIES];
	DIR *dir = opendir(path);
	struct dirent *entry;
	size_t count = 0;
	size_t index;
	int ok = 1;

	if (dir == NULL)
		return 0;
	for (;;) {
		long cookie = telldir(dir);
		entry = readdir(dir);
		if (entry == NULL)
			break;
		if (count >= MAX_ENTRIES) {
			ok = 0;
			break;
		}
		cookies[count] = cookie;
		snprintf(entries[count].name, sizeof(entries[count].name), "%s",
		    entry->d_name);
		entries[count].ino = entry->d_ino;
		count++;
	}
	if (errno != 0 || count != FILE_COUNT + 2)
		ok = 0;

	/* 137 is coprime to 222, so this replays every saved position in a
	 * deterministic non-sequential order and crosses every refill boundary. */
	for (index = 0; ok && index < count; index++) {
		size_t position = (index * 137) % count;
		seekdir(dir, cookies[position]);
		entry = readdir(dir);
		if (entry == NULL || entry->d_ino != entries[position].ino ||
		    strcmp(entry->d_name, entries[position].name) != 0)
			ok = 0;
	}
	(void)closedir(dir);
	return ok;
}

static int verify_short_buffer_cookie(const char *path) {
#ifdef __wasi__
	unsigned char short_buffer[25];
	unsigned char full_buffer[64];
	__wasi_dirent_t entry;
	__wasi_size_t used = 0;
	int fd = open(path, O_RDONLY | O_DIRECTORY);
	int ok = 1;

	if (fd < 0)
		return 0;
	memset(short_buffer, 0, sizeof(short_buffer));
	if (__wasi_fd_readdir((__wasi_fd_t)fd, short_buffer,
	    sizeof(short_buffer), 0, &used) != __WASI_ERRNO_SUCCESS ||
	    used != sizeof(short_buffer))
		ok = 0;
	memcpy(&entry, short_buffer, sizeof(entry));
	if (entry.d_next != 1 || entry.d_ino == 0 || entry.d_namlen != 1 ||
	    short_buffer[sizeof(entry)] != '.')
		ok = 0;

	memset(short_buffer, 0, sizeof(short_buffer));
	used = 0;
	if (__wasi_fd_readdir((__wasi_fd_t)fd, short_buffer,
	    sizeof(short_buffer), 1, &used) != __WASI_ERRNO_SUCCESS ||
	    used != sizeof(short_buffer))
		ok = 0;
	memcpy(&entry, short_buffer, sizeof(entry));
	if (entry.d_next != 2 || entry.d_ino == 0 || entry.d_namlen != 2 ||
	    short_buffer[sizeof(entry)] != '.')
		ok = 0;

	memset(full_buffer, 0, sizeof(full_buffer));
	used = 0;
	if (__wasi_fd_readdir((__wasi_fd_t)fd, full_buffer,
	    sizeof(full_buffer), 1, &used) != __WASI_ERRNO_SUCCESS ||
	    used < sizeof(entry) + 2)
		ok = 0;
	memcpy(&entry, full_buffer, sizeof(entry));
	if (entry.d_next != 2 || entry.d_ino == 0 || entry.d_namlen != 2 ||
	    memcmp(full_buffer + sizeof(entry), "..", 2) != 0)
		ok = 0;
	(void)close(fd);
	return ok;
#else
	(void)path;
	return 1;
#endif
}

static int verify_removed_open_directory(const char *base) {
	char path[192], proc_path[64], target[256], expected_target[256];
	struct stat before, after;
	struct dirent *entry;
	DIR *dir;
	ssize_t target_len;
	int ok, deleted_link = 0;

	snprintf(path, sizeof(path), "%s-detached", base);
	(void)rmdir(path);
	if (mkdir(path, 0700) != 0) {
		fprintf(stderr, "detached: mkdir errno=%d\n", errno);
		return 0;
	}
	dir = opendir(path);
	if (dir == NULL) {
		fprintf(stderr, "detached: opendir errno=%d\n", errno);
		(void)rmdir(path);
		return 0;
	}
	if (fstat(dirfd(dir), &before) != 0) {
		fprintf(stderr, "detached: initial fstat errno=%d\n", errno);
		closedir(dir);
		(void)rmdir(path);
		return 0;
	}
	if (rmdir(path) != 0) {
		fprintf(stderr, "detached: rmdir errno=%d\n", errno);
		if (dir != NULL)
			closedir(dir);
		(void)rmdir(path);
		return 0;
	}
	snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", dirfd(dir));
	target_len = readlink(proc_path, target, sizeof(target) - 1);
	snprintf(expected_target, sizeof(expected_target), "%s (deleted)", path);
	if (target_len >= 0) {
		target[target_len] = '\0';
		deleted_link = strcmp(target, expected_target) == 0;
	}
	errno = 0;
	entry = readdir(dir);
	{
		int saved_errno = errno;
		int stat_result = fstat(dirfd(dir), &after);
		ok = stat_result == 0 && after.st_ino == before.st_ino &&
		    entry == NULL && saved_errno == 0 && deleted_link;
		if (!ok)
			fprintf(stderr,
			    "detached: fstat=%d before_ino=%llu after_ino=%llu "
			    "entry=%s errno=%d deleted_link=%d\n",
			    stat_result, (unsigned long long)before.st_ino,
			    stat_result == 0 ? (unsigned long long)after.st_ino : 0,
			    entry == NULL ? "null" : entry->d_name, saved_errno,
			    deleted_link);
	}
	(void)closedir(dir);
	return ok;
}

static int verify_unlinked_open_file(const char *base) {
	char path[192], proc_path[64], target[256], expected_target[256];
	struct stat before, after;
	ssize_t target_len;
	int fd, ok;

	snprintf(path, sizeof(path), "%s-deleted-file", base);
	(void)unlink(path);
	fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0600);
	if (fd < 0 || write(fd, "x", 1) != 1 || fstat(fd, &before) != 0 ||
	    unlink(path) != 0 || fstat(fd, &after) != 0) {
		if (fd >= 0)
			close(fd);
		return 0;
	}
	snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", fd);
	target_len = readlink(proc_path, target, sizeof(target) - 1);
	snprintf(expected_target, sizeof(expected_target), "%s (deleted)", path);
	if (target_len >= 0)
		target[target_len] = '\0';
	ok = before.st_ino == after.st_ino && target_len >= 0 &&
	    strcmp(target, expected_target) == 0;
	(void)close(fd);
	return ok;
}

static int verify_renamed_open_directory(const char *base) {
	char before_path[192], after_path[192], child_path[256];
	struct stat before, after, child;
	struct dirent *entry;
	DIR *dir;
	int fd;
	int saw_child = 0;

	snprintf(before_path, sizeof(before_path), "%s-rename-before", base);
	snprintf(after_path, sizeof(after_path), "%s-rename-after", base);
	snprintf(child_path, sizeof(child_path), "%s/child", before_path);
	(void)unlink(child_path);
	(void)rmdir(before_path);
	snprintf(child_path, sizeof(child_path), "%s/child", after_path);
	(void)unlink(child_path);
	(void)rmdir(after_path);
	if (mkdir(before_path, 0700) != 0)
		return 0;
	snprintf(child_path, sizeof(child_path), "%s/child", before_path);
	fd = open(child_path, O_CREAT | O_WRONLY | O_TRUNC, 0600);
	if (fd < 0 || close(fd) != 0)
		return 0;
	fd = open(before_path, O_RDONLY | O_DIRECTORY);
	if (fd < 0 || fstat(fd, &before) != 0 ||
	    rename(before_path, after_path) != 0 || fstat(fd, &after) != 0 ||
	    fstatat(fd, "child", &child, AT_SYMLINK_NOFOLLOW) != 0) {
		if (fd >= 0)
			close(fd);
		return 0;
	}
	dir = fdopendir(fd);
	if (dir == NULL) {
		close(fd);
		return 0;
	}
	while ((entry = readdir(dir)) != NULL) {
		if (strcmp(entry->d_name, "child") == 0)
			saw_child = 1;
	}
	(void)closedir(dir);
	snprintf(child_path, sizeof(child_path), "%s/child", after_path);
	(void)unlink(child_path);
	(void)rmdir(after_path);
	return before.st_ino == after.st_ino && saw_child;
}

static int verify_fdopendir_first_read_and_eof(const char *base) {
	char directory[192], child_path[256];
	struct dirent *entry;
	DIR *dir = NULL;
	int fd = -1;
	int dots = 0, entries = 0, saw_child = 0;
	int first_eof_errno, repeated_eof_errno;
	int ok = 0;

	snprintf(directory, sizeof(directory), "%s-fdopendir-lazy", base);
	snprintf(child_path, sizeof(child_path), "%s/child", directory);
	(void)unlink(child_path);
	(void)rmdir(directory);
	if (mkdir(directory, 0700) != 0)
		goto cleanup;
	fd = open(child_path, O_CREAT | O_WRONLY | O_TRUNC, 0600);
	if (fd < 0 || close(fd) != 0)
		goto cleanup;
	fd = open(directory, O_RDONLY | O_DIRECTORY);
	if (fd < 0)
		goto cleanup;
	dir = fdopendir(fd);
	if (dir == NULL)
		goto cleanup;
	fd = -1;

#ifdef __wasi__
	/* A deferred fdopendir must make the first readdir refill before parsing
	 * this buffer. Assert the sentinel directly, then poison the buffer so a
	 * future regression cannot pass merely because malloc returned zeros. */
	if (dir->buffer_processed != dir->buffer_size ||
	    dir->buffer_used != dir->buffer_size)
		goto cleanup;
	memset(dir->buffer, 0xff, dir->buffer_size);
#endif

	errno = 0;
	while ((entry = readdir(dir)) != NULL) {
		entries++;
		if (entries > 3)
			goto cleanup;
		if (strcmp(entry->d_name, ".") == 0 ||
		    strcmp(entry->d_name, "..") == 0)
			dots++;
		if (strcmp(entry->d_name, "child") == 0)
			saw_child = 1;
	}
	first_eof_errno = errno;
	errno = 0;
	entry = readdir(dir);
	repeated_eof_errno = errno;
	ok = entries == 3 && dots == 2 && saw_child && first_eof_errno == 0 &&
	    entry == NULL && repeated_eof_errno == 0;

cleanup:
	if (dir != NULL)
		(void)closedir(dir);
	else if (fd >= 0)
		(void)close(fd);
	(void)unlink(child_path);
	(void)rmdir(directory);
	return ok;
}

int main(void) {
	char directory[128];
	char path[192];
	struct observed_entry *first;
	struct observed_entry *second;
	size_t first_count = 0, second_count = 0;
	int first_dots = 0, second_dots = 0;
	int first_nonzero = 0, second_nonzero = 0;
	int first_matches_stat = 0, second_matches_stat = 0;
	int stable = 1;
	int seek_ok;
	int all_seek_positions_ok;
	int linux_struct_capacity_ok;
	int short_buffer_ok;
	int detached_ok;
	int deleted_file_ok;
	int renamed_ok;
	int fdopendir_first_read_ok;
	int index;

	first = calloc(MAX_ENTRIES, sizeof(*first));
	second = calloc(MAX_ENTRIES, sizeof(*second));
	if (first == NULL || second == NULL) {
		free(first);
		free(second);
		perror("calloc");
		return 1;
	}

	(void)mkdir("/tmp", 0777);
	snprintf(directory, sizeof(directory), "/tmp/readdir-contract");
	(void)rmdir(directory);
	if (mkdir(directory, 0700) != 0) {
		perror("mkdir");
		return 1;
	}
	detached_ok = verify_removed_open_directory(directory);
	deleted_file_ok = verify_unlinked_open_file(directory);
	renamed_ok = verify_renamed_open_directory(directory);
	fdopendir_first_read_ok = verify_fdopendir_first_read_and_eof(directory);
	for (index = 0; index < FILE_COUNT; index++) {
		int fd;
		snprintf(path, sizeof(path), "%s/entry-%03d", directory, index);
		fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0600);
		if (fd < 0 || close(fd) != 0) {
			perror("create entry");
			return 1;
		}
	}

	seek_ok = verify_seekdir(directory);
	all_seek_positions_ok = verify_all_seekdir_positions(directory);
	linux_struct_capacity_ok = sizeof(((struct dirent *)0)->d_name) >= 256 &&
	    sizeof(struct dirent) >= offsetof(struct dirent, d_name) + 256;
	short_buffer_ok = verify_short_buffer_cookie(directory);
	if (collect(directory, first, &first_count, &first_dots,
	    &first_nonzero, &first_matches_stat) != 0 ||
	    collect(directory, second, &second_count, &second_dots,
	    &second_nonzero, &second_matches_stat) != 0) {
		perror("readdir");
		return 1;
	}
	if (first_count != second_count)
		stable = 0;
	for (index = 0; stable && (size_t)index < first_count; index++) {
		if (strcmp(first[index].name, second[index].name) != 0 ||
		    first[index].ino != second[index].ino)
			stable = 0;
	}

	printf("readdir_dots=%s\n", first_dots == 2 && second_dots == 2 ? "yes" : "no");
	printf("readdir_nonzero_ino=%s\n", first_nonzero && second_nonzero ? "yes" : "no");
	printf("readdir_ino_matches_stat=%s\n",
	    first_matches_stat && second_matches_stat ? "yes" : "no");
	printf("readdir_seekdir_resume=%s\n", seek_ok ? "yes" : "no");
	printf("readdir_all_seekdir_positions=%s\n",
	    all_seek_positions_ok ? "yes" : "no");
	printf("readdir_linux_struct_capacity=%s\n",
	    linux_struct_capacity_ok ? "yes" : "no");
	printf("readdir_short_buffer_cookie=%s\n", short_buffer_ok ? "yes" : "no");
	printf("readdir_stable_ino=%s\n", stable ? "yes" : "no");
	printf("readdir_detached_directory=%s\n", detached_ok ? "yes" : "no");
	printf("proc_deleted_file=%s\n", deleted_file_ok ? "yes" : "no");
	printf("readdir_renamed_directory=%s\n", renamed_ok ? "yes" : "no");
	printf("readdir_fdopendir_first_read_and_eof=%s\n",
	    fdopendir_first_read_ok ? "yes" : "no");
	printf("readdir_count=%zu\n", first_count);

	for (index = 0; index < FILE_COUNT; index++) {
		snprintf(path, sizeof(path), "%s/entry-%03d", directory, index);
		(void)unlink(path);
	}
	(void)rmdir(directory);

	if (first_dots != 2 || second_dots != 2 || !first_nonzero ||
	    !second_nonzero || !first_matches_stat || !second_matches_stat ||
	    !seek_ok || !all_seek_positions_ok || !linux_struct_capacity_ok ||
	    !short_buffer_ok || !stable || !detached_ok ||
	    !deleted_file_ok || !renamed_ok || !fdopendir_first_read_ok ||
	    first_count != FILE_COUNT + 2) {
		puts("readdir_contract=failed");
		free(first);
		free(second);
		return 1;
	}
	puts("readdir_contract=ok");
	free(first);
	free(second);
	return 0;
}
