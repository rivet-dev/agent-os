/* sqlite3_cli — SQLite CLI for WasmVM
 *
 * Full sqlite3 CLI supporting:
 *   sqlite3 :memory:              — interactive/piped in-memory DB
 *   sqlite3 /path/to/db.sqlite    — file-based DB via WASI VFS
 *   echo 'SELECT 1;' | sqlite3    — stdin pipe mode (defaults to :memory:)
 *   sqlite3 :memory: "SELECT 1;"  — SQL from command line argument
 *
 * Meta-commands: .dump, .schema, .tables, .quit, .help, .headers, .mode
 *
 * Compiled with -DSQLITE_OS_OTHER — uses a custom VFS wrapping WASI file I/O.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include "sqlite3.h"

/* ── WASI VFS ────────────────────────────────────────────────────────── */

/* Minimal VFS implementation using POSIX file I/O (supported by WASI) */

typedef struct WasiFile {
    sqlite3_file base;
    int fd;
} WasiFile;

static int wasiClose(sqlite3_file *pFile) {
    WasiFile *p = (WasiFile *)pFile;
    if (p->fd >= 0) close(p->fd);
    p->fd = -1;
    return SQLITE_OK;
}

static int wasiRead(sqlite3_file *pFile, void *zBuf, int iAmt, sqlite3_int64 iOfst) {
    WasiFile *p = (WasiFile *)pFile;
    if (lseek(p->fd, (off_t)iOfst, SEEK_SET) != (off_t)iOfst)
        return SQLITE_IOERR_READ;
    ssize_t got = read(p->fd, zBuf, iAmt);
    if (got == iAmt) return SQLITE_OK;
    if (got >= 0) {
        memset((char *)zBuf + got, 0, iAmt - got);
        return SQLITE_IOERR_SHORT_READ;
    }
    return SQLITE_IOERR_READ;
}

static int wasiWrite(sqlite3_file *pFile, const void *zBuf, int iAmt, sqlite3_int64 iOfst) {
    WasiFile *p = (WasiFile *)pFile;
    if (lseek(p->fd, (off_t)iOfst, SEEK_SET) != (off_t)iOfst)
        return SQLITE_IOERR_WRITE;
    ssize_t wrote = write(p->fd, zBuf, iAmt);
    if (wrote == iAmt) return SQLITE_OK;
    return SQLITE_IOERR_WRITE;
}

static int wasiTruncate(sqlite3_file *pFile, sqlite3_int64 size) {
    WasiFile *p = (WasiFile *)pFile;
    if (ftruncate(p->fd, (off_t)size) != 0) return SQLITE_IOERR_TRUNCATE;
    return SQLITE_OK;
}

static int wasiSync(sqlite3_file *pFile, int flags) {
    (void)flags;
    WasiFile *p = (WasiFile *)pFile;
    if (fsync(p->fd) != 0) return SQLITE_IOERR_FSYNC;
    return SQLITE_OK;
}

static int wasiFileSize(sqlite3_file *pFile, sqlite3_int64 *pSize) {
    WasiFile *p = (WasiFile *)pFile;
    struct stat st;
    if (fstat(p->fd, &st) != 0) return SQLITE_IOERR_FSTAT;
    *pSize = (sqlite3_int64)st.st_size;
    return SQLITE_OK;
}

/* No locking in WASM — single process */
static int wasiLock(sqlite3_file *f, int l) { (void)f; (void)l; return SQLITE_OK; }
static int wasiUnlock(sqlite3_file *f, int l) { (void)f; (void)l; return SQLITE_OK; }
static int wasiCheckReservedLock(sqlite3_file *f, int *pResOut) {
    (void)f; *pResOut = 0; return SQLITE_OK;
}
static int wasiFileControl(sqlite3_file *f, int op, void *pArg) {
    (void)f; (void)op; (void)pArg; return SQLITE_NOTFOUND;
}
static int wasiSectorSize(sqlite3_file *f) { (void)f; return 512; }
static int wasiDeviceCharacteristics(sqlite3_file *f) { (void)f; return 0; }

static const sqlite3_io_methods wasiIoMethods __attribute__((used)) = {
    1,                          /* iVersion */
    wasiClose,
    wasiRead,
    wasiWrite,
    wasiTruncate,
    wasiSync,
    wasiFileSize,
    wasiLock,
    wasiUnlock,
    wasiCheckReservedLock,
    wasiFileControl,
    wasiSectorSize,
    wasiDeviceCharacteristics,
    /* v2+ methods */
    0, 0, 0, 0
};

static int wasiOpen(sqlite3_vfs *pVfs, sqlite3_filename zName, sqlite3_file *pFile,
                    int flags, int *pOutFlags) {
    (void)pVfs;
    WasiFile *p = (WasiFile *)pFile;
    p->fd = -1;

    if (!zName) {
        /* Temp file — use in-memory only */
        return SQLITE_IOERR;
    }

    int oflags = 0;
    if (flags & SQLITE_OPEN_CREATE) oflags |= O_CREAT;
    if (flags & SQLITE_OPEN_READWRITE) oflags |= O_RDWR;
    else oflags |= O_RDONLY;

    p->fd = open(zName, oflags, 0644);
    if (p->fd < 0) return SQLITE_CANTOPEN;

    if (pOutFlags) *pOutFlags = flags;
    p->base.pMethods = &wasiIoMethods;
    return SQLITE_OK;
}

static int wasiDelete(sqlite3_vfs *pVfs, const char *zName, int syncDir) {
    (void)pVfs; (void)syncDir;
    if (unlink(zName) != 0 && errno != ENOENT) return SQLITE_IOERR_DELETE;
    return SQLITE_OK;
}

static int wasiAccess(sqlite3_vfs *pVfs, const char *zName, int flags, int *pResOut) {
    (void)pVfs;
    struct stat st;
    *pResOut = (stat(zName, &st) == 0) ? 1 : 0;
    (void)flags;
    return SQLITE_OK;
}

static int wasiFullPathname(sqlite3_vfs *pVfs, const char *zName, int nOut, char *zOut) {
    (void)pVfs;
    if (zName[0] == '/') {
        snprintf(zOut, nOut, "%s", zName);
    } else {
        /* Relative path — prefix with / since VFS root is / */
        snprintf(zOut, nOut, "/%s", zName);
    }
    return SQLITE_OK;
}

static int wasiRandomness(sqlite3_vfs *pVfs, int nByte, char *zOut) {
    (void)pVfs;
    int fd = open("/dev/urandom", O_RDONLY);
    if (fd >= 0) {
        int total = 0;
        while (total < nByte) {
            ssize_t read_len = read(fd, zOut + total, (size_t)(nByte - total));
            if (read_len <= 0) {
                break;
            }
            total += (int)read_len;
        }
        close(fd);
        if (total == nByte) {
            return nByte;
        }
        nByte = total;
    }

    /* Fallback only if urandom is unexpectedly unavailable in the runtime. */
    for (int i = 0; i < nByte; i++) zOut[i] = (char)(i * 37 + 17);
    return nByte;
}

static int wasiSleep(sqlite3_vfs *pVfs, int microseconds) {
    (void)pVfs; (void)microseconds;
    return 0;
}

static int wasiCurrentTime(sqlite3_vfs *pVfs, double *pTime) {
    (void)pVfs;
    /* Julian day for Unix epoch 0 */
    *pTime = 2440587.5;
    return SQLITE_OK;
}

static int wasiGetLastError(sqlite3_vfs *pVfs, int nBuf, char *zBuf) {
    (void)pVfs;
    if (nBuf > 0) zBuf[0] = 0;
    return 0;
}

static sqlite3_vfs wasiVfs __attribute__((used)) = {
    1,                    /* iVersion */
    sizeof(WasiFile),     /* szOsFile */
    512,                  /* mxPathname */
    0,                    /* pNext */
    "wasi",               /* zName */
    0,                    /* pAppData */
    wasiOpen,
    wasiDelete,
    wasiAccess,
    wasiFullPathname,
    0, 0, 0, 0,          /* xDlOpen, xDlError, xDlSym, xDlClose */
    wasiRandomness,
    wasiSleep,
    wasiCurrentTime,
    wasiGetLastError,
    /* v2+ */
    0, 0, 0, 0
};

#ifdef SQLITE_OS_OTHER
int sqlite3_os_init(void) {
    return sqlite3_vfs_register(&wasiVfs, 1);
}

int sqlite3_os_end(void) {
    return SQLITE_OK;
}
#endif /* SQLITE_OS_OTHER */

/* ── CLI ─────────────────────────────────────────────────────────────── */

static int headers_enabled = 0;
static int headers_printed = 0;
static char output_sep_buf[8];
static char *output_separator = output_sep_buf;

/* Callback for sqlite3_exec — prints rows in column format */
static int print_row(void *unused, int ncols, char **values, char **names) {
    (void)unused;
    if (headers_enabled && !headers_printed) {
        for (int i = 0; i < ncols; i++) {
            if (i > 0) printf("%s", output_separator);
            printf("%s", names[i]);
        }
        printf("\n");
        headers_printed = 1;
    }
    for (int i = 0; i < ncols; i++) {
        if (i > 0) printf("%s", output_separator);
        printf("%s", values[i] ? values[i] : "");
    }
    printf("\n");
    return 0;
}

/* Execute SQL and report errors */
static int exec_sql(sqlite3 *db, const char *sql) {
    char *err = NULL;
    headers_printed = 0; /* Reset per query */
    int rc = sqlite3_exec(db, sql, print_row, NULL, &err);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "Error: %s\n", err ? err : sqlite3_errmsg(db));
        sqlite3_free(err);
        return 1;
    }
    return 0;
}

/* Handle dot-commands */
static int handle_meta(sqlite3 *db, const char *line) {
    while (*line == ' ' || *line == '\t') line++;

    if (strcmp(line, ".quit") == 0 || strcmp(line, ".exit") == 0) {
        return -1; /* Signal exit */
    }
    if (strcmp(line, ".help") == 0) {
        printf(".dump              Dump database as SQL\n");
        printf(".headers on|off    Toggle column headers\n");
        printf(".help              Show this help\n");
        printf(".mode MODE         Set output mode (list, csv)\n");
        printf(".quit              Exit\n");
        printf(".schema            Show CREATE statements\n");
        printf(".tables            List tables\n");
        return 0;
    }
    if (strcmp(line, ".tables") == 0) {
        return exec_sql(db,
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1;");
    }
    if (strcmp(line, ".schema") == 0) {
        return exec_sql(db,
            "SELECT sql||';' FROM sqlite_master WHERE sql IS NOT NULL ORDER BY 1;");
    }
    if (strcmp(line, ".dump") == 0) {
        printf("BEGIN TRANSACTION;\n");
        /* Schema */
        exec_sql(db,
            "SELECT sql||';' FROM sqlite_master WHERE sql IS NOT NULL "
            "ORDER BY CASE WHEN type='table' THEN 0 ELSE 1 END, name;");
        /* Data — generate INSERT statements for each table */
        sqlite3_stmt *stmt;
        int rc = sqlite3_prepare_v2(db,
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1;",
            -1, &stmt, NULL);
        if (rc == SQLITE_OK) {
            while (sqlite3_step(stmt) == SQLITE_ROW) {
                const char *table = (const char *)sqlite3_column_text(stmt, 0);
                char query[1024];
                snprintf(query, sizeof(query), "SELECT * FROM \"%s\"", table);

                sqlite3_stmt *data_stmt;
                rc = sqlite3_prepare_v2(db, query, -1, &data_stmt, NULL);
                if (rc == SQLITE_OK) {
                    int ncols = sqlite3_column_count(data_stmt);
                    while (sqlite3_step(data_stmt) == SQLITE_ROW) {
                        printf("INSERT INTO \"%s\" VALUES(", table);
                        for (int i = 0; i < ncols; i++) {
                            if (i > 0) printf(",");
                            int type = sqlite3_column_type(data_stmt, i);
                            if (type == SQLITE_NULL) {
                                printf("NULL");
                            } else if (type == SQLITE_INTEGER) {
                                printf("%lld", sqlite3_column_int64(data_stmt, i));
                            } else if (type == SQLITE_FLOAT) {
                                printf("%g", sqlite3_column_double(data_stmt, i));
                            } else {
                                const char *text = (const char *)sqlite3_column_text(data_stmt, i);
                                printf("'");
                                for (const char *c = text; *c; c++) {
                                    if (*c == '\'') printf("''");
                                    else putchar(*c);
                                }
                                printf("'");
                            }
                        }
                        printf(");\n");
                    }
                    sqlite3_finalize(data_stmt);
                }
            }
            sqlite3_finalize(stmt);
        }
        printf("COMMIT;\n");
        return 0;
    }
    if (strncmp(line, ".headers ", 9) == 0) {
        const char *arg = line + 9;
        while (*arg == ' ') arg++;
        if (strcmp(arg, "on") == 0) headers_enabled = 1;
        else headers_enabled = 0;
        return 0;
    }
    if (strncmp(line, ".mode ", 6) == 0) {
        const char *arg = line + 6;
        while (*arg == ' ') arg++;
        if (strcmp(arg, "csv") == 0) strcpy(output_sep_buf, ",");
        else strcpy(output_sep_buf, "|");
        return 0;
    }

    fprintf(stderr, "Error: unknown command or invalid arguments: \"%s\"\n", line);
    return 0;
}

/* Read all data from stream into a buffer */
static char *read_all(FILE *in, size_t *out_len) {
    size_t cap = 65536;
    size_t len = 0;
    char *buf = (char *)malloc(cap);
    if (!buf) return NULL;

    int c;
    while ((c = fgetc(in)) != EOF) {
        if (len + 1 >= cap) {
            cap *= 2;
            char *nb = (char *)realloc(buf, cap);
            if (!nb) { free(buf); return NULL; }
            buf = nb;
        }
        buf[len++] = (char)c;
    }
    buf[len] = '\0';
    if (out_len) *out_len = len;
    return buf;
}

/* Process a line of input — either meta-command or SQL accumulation */
static int process_line(sqlite3 *db, const char *line, size_t len,
                        char *sql_buf, int *sql_len, int sql_cap) {
    /* Meta-commands start with . */
    if (line[0] == '.' && *sql_len == 0) {
        /* Need a mutable copy for handle_meta */
        char meta_buf[1024];
        size_t ml = len < sizeof(meta_buf) - 1 ? len : sizeof(meta_buf) - 1;
        memcpy(meta_buf, line, ml);
        meta_buf[ml] = '\0';
        int rc = handle_meta(db, meta_buf);
        if (rc < 0) return -1; /* .quit */
        return rc;
    }

    /* Accumulate SQL */
    if (*sql_len + (int)len + 2 < sql_cap) {
        if (*sql_len > 0) sql_buf[(*sql_len)++] = '\n';
        memcpy(sql_buf + *sql_len, line, len);
        *sql_len += (int)len;
        sql_buf[*sql_len] = '\0';
    }

    /* Check if statement is complete */
    if (sqlite3_complete(sql_buf)) {
        int rc = exec_sql(db, sql_buf);
        *sql_len = 0;
        return rc;
    }
    return 0;
}

/* Read and execute SQL from stdin */
static int process_input(sqlite3 *db, FILE *in) {
    size_t total = 0;
    char *data = read_all(in, &total);
    if (!data || total == 0) {
        free(data);
        return 0;
    }

    char sql_buf[65536];
    int sql_len = 0;
    int had_error = 0;

    /* Process input line by line */
    const char *p = data;
    const char *end = data + total;
    while (p < end) {
        /* Find end of line */
        const char *eol = p;
        while (eol < end && *eol != '\n' && *eol != '\r') eol++;
        size_t len = eol - p;

        /* Skip empty lines */
        if (len > 0) {
            int rc = process_line(db, p, len, sql_buf, &sql_len, (int)sizeof(sql_buf));
            if (rc < 0) break; /* .quit */
            if (rc > 0) had_error = 1;
        }

        /* Skip past newline */
        p = eol;
        if (p < end && *p == '\r') p++;
        if (p < end && *p == '\n') p++;
    }

    /* Execute any remaining SQL */
    if (sql_len > 0) {
        if (exec_sql(db, sql_buf) != 0)
            had_error = 1;
    }

    free(data);
    return had_error;
}

static void print_usage(void) {
    fprintf(stderr, "Usage: sqlite3 [OPTIONS] [FILENAME] [SQL]\n");
    fprintf(stderr, "  FILENAME is the name of an SQLite database. A new database is\n");
    fprintf(stderr, "  created if the file does not exist. Use \":memory:\" for in-memory.\n");
    fprintf(stderr, "  SQL is an optional SQL statement to execute.\n");
    fprintf(stderr, "Options:\n");
    fprintf(stderr, "  -header       Turn headers on\n");
    fprintf(stderr, "  -csv          Set output mode to CSV\n");
    fprintf(stderr, "  -separator S  Set output separator (default \"|\")\n");
    fprintf(stderr, "  -help         Show this help\n");
    fprintf(stderr, "  -version      Show SQLite version\n");
}

int main(int argc, char **argv) {
    const char *db_path = ":memory:";
    const char *sql_arg = NULL;
    int positional = 0;
    int i;

    /* Initialize separator at runtime — static initializers for arrays
     * may not work correctly in optimized WASM builds. */
    output_sep_buf[0] = '|';
    output_sep_buf[1] = '\0';

    /* Parse options */
    for (i = 1; i < argc; i++) {
        if (argv[i][0] == '-') {
            if (strcmp(argv[i], "-help") == 0 || strcmp(argv[i], "--help") == 0) {
                print_usage();
                return 0;
            }
            if (strcmp(argv[i], "-version") == 0 || strcmp(argv[i], "--version") == 0) {
                printf("%s\n", sqlite3_libversion());
                return 0;
            }
            if (strcmp(argv[i], "-header") == 0 || strcmp(argv[i], "-headers") == 0) {
                headers_enabled = 1;
                continue;
            }
            if (strcmp(argv[i], "-csv") == 0) {
                strcpy(output_sep_buf, ",");
                continue;
            }
            if (strcmp(argv[i], "-separator") == 0 && i + 1 < argc) {
                strncpy(output_sep_buf, argv[++i], sizeof(output_sep_buf) - 1);
                output_sep_buf[sizeof(output_sep_buf) - 1] = '\0';
                continue;
            }
            fprintf(stderr, "Error: unknown option: %s\n", argv[i]);
            print_usage();
            return 1;
        }
        /* Positional args: [FILENAME] [SQL] */
        if (positional == 0) {
            db_path = argv[i];
            positional++;
        } else if (positional == 1) {
            sql_arg = argv[i];
            positional++;
        } else {
            fprintf(stderr, "Error: too many arguments\n");
            print_usage();
            return 1;
        }
    }

    /* Open database */
    sqlite3 *db;
    int rc = sqlite3_open(db_path, &db);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "Error: unable to open database \"%s\": %s\n",
                db_path, sqlite3_errmsg(db));
        sqlite3_close(db);
        return 1;
    }

    int exit_code = 0;

    if (sql_arg) {
        /* Execute SQL from command line */
        exit_code = exec_sql(db, sql_arg);
    } else {
        /* Read from stdin */
        exit_code = process_input(db, stdin);
    }

    /* Flush output */
    fflush(stdout);
    fflush(stderr);
    /* Skip sqlite3_close + atexit handlers — use _Exit to avoid WASM indirect
     * call table traps during wasi-libc/SQLite cleanup with wasm-opt. */
    _Exit(exit_code);
}
