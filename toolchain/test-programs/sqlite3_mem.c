/* sqlite3_mem — in-memory SQLite operations (Tier 5 test fixture)
 *
 * Creates an in-memory database, creates a table, inserts rows,
 * queries them, and prints the results. Validates the full libc surface
 * (malloc, string formatting, math) exercised by SQLite.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"

#ifdef SQLITE_OS_OTHER
/* SQLite still requires a default VFS for :memory: databases. */
typedef struct MemFile {
    sqlite3_file base;
} MemFile;

static int memClose(sqlite3_file *pFile) { (void)pFile; return SQLITE_OK; }
static int memRead(sqlite3_file *pFile, void *zBuf, int iAmt, sqlite3_int64 iOfst) {
    (void)pFile; (void)zBuf; (void)iAmt; (void)iOfst; return SQLITE_IOERR_READ;
}
static int memWrite(sqlite3_file *pFile, const void *zBuf, int iAmt, sqlite3_int64 iOfst) {
    (void)pFile; (void)zBuf; (void)iAmt; (void)iOfst; return SQLITE_IOERR_WRITE;
}
static int memTruncate(sqlite3_file *pFile, sqlite3_int64 size) {
    (void)pFile; (void)size; return SQLITE_IOERR_TRUNCATE;
}
static int memSync(sqlite3_file *pFile, int flags) {
    (void)pFile; (void)flags; return SQLITE_OK;
}
static int memFileSize(sqlite3_file *pFile, sqlite3_int64 *pSize) {
    (void)pFile; *pSize = 0; return SQLITE_OK;
}
static int memLock(sqlite3_file *pFile, int lock) {
    (void)pFile; (void)lock; return SQLITE_OK;
}
static int memUnlock(sqlite3_file *pFile, int lock) {
    (void)pFile; (void)lock; return SQLITE_OK;
}
static int memCheckReservedLock(sqlite3_file *pFile, int *pResOut) {
    (void)pFile; *pResOut = 0; return SQLITE_OK;
}
static int memFileControl(sqlite3_file *pFile, int op, void *pArg) {
    (void)pFile; (void)op; (void)pArg; return SQLITE_NOTFOUND;
}
static int memSectorSize(sqlite3_file *pFile) { (void)pFile; return 512; }
static int memDeviceCharacteristics(sqlite3_file *pFile) { (void)pFile; return 0; }

static const sqlite3_io_methods memIoMethods __attribute__((used)) = {
    1,
    memClose,
    memRead,
    memWrite,
    memTruncate,
    memSync,
    memFileSize,
    memLock,
    memUnlock,
    memCheckReservedLock,
    memFileControl,
    memSectorSize,
    memDeviceCharacteristics,
    0, 0, 0, 0
};

static int memOpen(sqlite3_vfs *pVfs, sqlite3_filename zName, sqlite3_file *pFile,
                   int flags, int *pOutFlags) {
    (void)pVfs; (void)zName; (void)flags;
    pFile->pMethods = 0;
    if (pOutFlags) *pOutFlags = flags;
    return SQLITE_CANTOPEN;
}
static int memDelete(sqlite3_vfs *pVfs, const char *zName, int syncDir) {
    (void)pVfs; (void)zName; (void)syncDir; return SQLITE_OK;
}
static int memAccess(sqlite3_vfs *pVfs, const char *zName, int flags, int *pResOut) {
    (void)pVfs; (void)zName; (void)flags; *pResOut = 0; return SQLITE_OK;
}
static int memFullPathname(sqlite3_vfs *pVfs, const char *zName, int nOut, char *zOut) {
    (void)pVfs;
    snprintf(zOut, (size_t)nOut, "%s", zName ? zName : ":memory:");
    return SQLITE_OK;
}
static int memRandomness(sqlite3_vfs *pVfs, int nByte, char *zOut) {
    (void)pVfs;
    for (int i = 0; i < nByte; i++) zOut[i] = (char)(i * 37 + 17);
    return nByte;
}
static int memSleep(sqlite3_vfs *pVfs, int microseconds) {
    (void)pVfs; (void)microseconds; return 0;
}
static int memCurrentTime(sqlite3_vfs *pVfs, double *pTime) {
    (void)pVfs; *pTime = 2440587.5; return SQLITE_OK;
}
static int memGetLastError(sqlite3_vfs *pVfs, int nBuf, char *zBuf) {
    (void)pVfs;
    if (nBuf > 0) zBuf[0] = 0;
    return 0;
}

static sqlite3_vfs memVfs __attribute__((used)) = {
    1,
    sizeof(MemFile),
    512,
    0,
    "agentos-mem",
    0,
    memOpen,
    memDelete,
    memAccess,
    memFullPathname,
    0, 0, 0, 0,
    memRandomness,
    memSleep,
    memCurrentTime,
    memGetLastError,
    0, 0, 0, 0
};

int sqlite3_os_init(void) { return sqlite3_vfs_register(&memVfs, 1); }
int sqlite3_os_end(void)  { return SQLITE_OK; }
#endif

static int print_row(void *unused, int ncols, char **values, char **names) {
    (void)unused;
    for (int i = 0; i < ncols; i++) {
        if (i > 0) printf("|");
        printf("%s=%s", names[i], values[i] ? values[i] : "NULL");
    }
    printf("\n");
    return 0;
}

static void exec_sql(sqlite3 *db, const char *sql, const char *label) {
    char *err = NULL;
    int rc = sqlite3_exec(db, sql, print_row, NULL, &err);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "%s error: %s\n", label, err);
        sqlite3_free(err);
        sqlite3_close(db);
        exit(1);
    }
}

int main(void) {
    sqlite3 *db;
    int rc;

    rc = sqlite3_open(":memory:", &db);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "open error: %s\n", sqlite3_errmsg(db));
        return 1;
    }
    printf("db: open\n");

    exec_sql(db,
        "CREATE TABLE users ("
        "  id INTEGER PRIMARY KEY,"
        "  name TEXT NOT NULL,"
        "  score REAL"
        ");",
        "create");
    printf("table: created\n");

    exec_sql(db,
        "INSERT INTO users VALUES (1, 'Alice', 95.5);"
        "INSERT INTO users VALUES (2, 'Bob', 87.3);"
        "INSERT INTO users VALUES (3, 'Charlie', NULL);"
        "INSERT INTO users VALUES (4, 'Diana', 92.1);",
        "insert");
    printf("rows: 4\n");

    printf("--- query: all ---\n");
    exec_sql(db, "SELECT id, name, score FROM users ORDER BY id;", "select");

    printf("--- query: avg ---\n");
    exec_sql(db, "SELECT COUNT(*) as total, AVG(score) as avg_score FROM users WHERE score IS NOT NULL;", "avg");

    printf("--- query: top ---\n");
    exec_sql(db, "SELECT name, score FROM users WHERE score IS NOT NULL ORDER BY score DESC LIMIT 2;", "top");

    printf("version: %s\n", sqlite3_libversion());

    sqlite3_close(db);
    printf("db: closed\n");

    return 0;
}
