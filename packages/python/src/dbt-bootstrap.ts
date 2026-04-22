/**
 * Bootstrap snippets that prepare a Pyodide worker for running dbt-core.
 *
 * These snippets are intended to be passed as `wheelPreload.bootstrapScript`
 * to either driver. They run *after* wheels are installed but *before* the
 * worker reports ready, so any subsequent `import dbt` sees a patched
 * environment.
 *
 * Why this is necessary:
 *   1. `dbt.mp_context` does `multiprocessing.get_context("spawn")` at
 *      import time. Pyodide's `multiprocessing` does not support spawn
 *      semantics — without a patch, this raises and the entire import
 *      fails. We replace `get_context` with a stub that returns a
 *      SimpleNamespace exposing the small surface dbt actually uses
 *      (Lock/RLock/Semaphore/Queue, all backed by `threading` primitives).
 *   2. `dbt.graph.thread_pool` imports `multiprocessing.pool.ThreadPool`
 *      at top level. The import itself is fine; the class can even be
 *      instantiated. But under Pyodide the `ThreadPool.apply_async`
 *      callback semantics deadlock because the event loop never yields
 *      back to the main coroutine while waiting on `pool.join()`. The
 *      mitigation is `DBT_SINGLE_THREADED=True`, which makes
 *      `GraphRunnableTask._submit` bypass the pool entirely. The env var
 *      must be set in `os.environ` before `dbt.task.runnable` is imported.
 */

/**
 * Python source executed at worker init. Idempotent — safe to run twice.
 */
export const DBT_BOOTSTRAP_SCRIPT = `# dbt + Pyodide bootstrap.
# See packages/python/src/dbt-bootstrap.ts for rationale.

import os
import sys
import threading
import types

# Replace concurrent.futures.ThreadPoolExecutor with a synchronous executor.
# Pyodide's CPython has no real OS threads — any ThreadPoolExecutor.submit()
# call would raise RuntimeError("can't start new thread"). dbt-core uses
# ThreadPoolExecutor for pre-run macro hooks even with --single-threaded
# (the flag only affects model execution). Patching the class itself makes
# all submit() calls run synchronously, returning a completed Future.
import concurrent.futures as _cf

if not getattr(_cf.ThreadPoolExecutor, "_agent_os_dbt_sync_patched", False):
    class _SyncFuture:
        """Minimal Future-like object for the sync-executor return value."""

        __slots__ = ("_result", "_exception")

        def __init__(self, result=None, exception=None):
            self._result = result
            self._exception = exception

        def result(self, timeout=None):
            if self._exception is not None:
                raise self._exception
            return self._result

        def exception(self, timeout=None):
            return self._exception

        def done(self):
            return True

        def cancelled(self):
            return False

        def add_done_callback(self, fn):
            try:
                fn(self)
            except Exception:
                pass

    class _SyncThreadPoolExecutor:
        """ThreadPoolExecutor stand-in that runs submitted callables synchronously."""

        _agent_os_dbt_sync_patched = True

        def __init__(self, *args, **kwargs):
            pass

        def submit(self, fn, *args, **kwargs):
            try:
                return _SyncFuture(result=fn(*args, **kwargs))
            except BaseException as e:
                return _SyncFuture(exception=e)

        def map(self, fn, *iterables, timeout=None, chunksize=1):
            return [fn(*items) for items in zip(*iterables)]

        def shutdown(self, wait=True, *, cancel_futures=False):
            pass

        def __enter__(self):
            return self

        def __exit__(self, *args):
            self.shutdown()
            return False

    _cf.ThreadPoolExecutor = _SyncThreadPoolExecutor

# Stub _multiprocessing BEFORE multiprocessing imports it. Pyodide does not
# ship the _multiprocessing C extension at all, so any \`from multiprocessing
# import X\` that touches the C side raises ModuleNotFoundError. We install
# a minimal stub that exposes the slice of the C API multiprocessing.queues /
# multiprocessing.synchronize need to import without raising.
if "_multiprocessing" not in sys.modules:
    _mp_stub = types.ModuleType("_multiprocessing")

    class _SemLock:
        """Threading-backed shim for the C SemLock used by mp.synchronize."""

        SEM_VALUE_MAX = 2147483647

        def __init__(self, kind, value, maxvalue, name=None, unlink=False):
            self._kind = kind
            self._value = value
            self._maxvalue = maxvalue
            self._sem = threading.Semaphore(value)
            self.handle = id(self)

        def acquire(self, block=True, timeout=None):
            return self._sem.acquire(blocking=block, timeout=timeout)

        def release(self):
            self._sem.release()

        def _count(self):
            return 0

        def _is_zero(self):
            return False

        def _is_mine(self):
            return True

        def _after_fork(self):
            pass

        def __enter__(self):
            self.acquire()
            return self

        def __exit__(self, *a):
            self.release()

    _mp_stub.SemLock = _SemLock
    _mp_stub.sem_unlink = lambda name: None
    _mp_stub.flags = types.SimpleNamespace(HAVE_BROKEN_SEM_GETVALUE=False)
    sys.modules["_multiprocessing"] = _mp_stub

import multiprocessing


def _make_spawn_context_stub():
    """Build a SimpleNamespace mimicking the slice of SpawnContext that dbt-core
    and dbt-adapters actually consume.

    The real SpawnContext's purpose is to give adapters a place to manufacture
    multiprocessing primitives. Under Pyodide we route those to threading
    primitives, which are correct *enough* given DBT_SINGLE_THREADED=True
    forces all dbt work onto the main coroutine anyway.
    """
    import queue as _queue
    return types.SimpleNamespace(
        Lock=threading.Lock,
        RLock=threading.RLock,
        Semaphore=threading.Semaphore,
        BoundedSemaphore=threading.BoundedSemaphore,
        Event=threading.Event,
        Condition=threading.Condition,
        Queue=lambda *a, **kw: _queue.Queue(*a, **kw),
        SimpleQueue=lambda: _queue.SimpleQueue(),
        JoinableQueue=lambda *a, **kw: _queue.Queue(*a, **kw),
        Pipe=lambda duplex=True: (None, None),
        Value=lambda typecode_or_type, *args, lock=True: types.SimpleNamespace(value=args[0] if args else 0),
        Array=lambda typecode_or_type, size_or_initializer, lock=True: list(range(size_or_initializer)) if isinstance(size_or_initializer, int) else list(size_or_initializer),
        # Manager() is rare in dbt and is intentionally unsupported here.
        Manager=lambda: (_ for _ in ()).throw(
            NotImplementedError(
                "multiprocessing.Manager is not supported in Pyodide"
            )
        ),
    )


_orig_get_context = multiprocessing.get_context


def _patched_get_context(method=None):
    if method is None:
        method = "spawn"  # dbt's default
    if method == "spawn":
        return _make_spawn_context_stub()
    # Fall back to original for any other context method.
    return _orig_get_context(method)


# Idempotency: only patch if our marker isn't already set.
if not getattr(multiprocessing, "_agent_os_dbt_patched", False):
    multiprocessing.get_context = _patched_get_context
    multiprocessing._agent_os_dbt_patched = True


# Force single-threaded execution before dbt.task.runnable is imported.
# DBT_SINGLE_THREADED is read at runnable-task construction time, but
# we set it eagerly so any path that observes it sees the right value.
os.environ.setdefault("DBT_SINGLE_THREADED", "True")
os.environ.setdefault("DBT_SEND_ANONYMOUS_USAGE_STATS", "False")
os.environ.setdefault("DBT_STATIC_PARSER", "False")
os.environ.setdefault("DBT_USE_EXPERIMENTAL_PARSER", "False")
os.environ.setdefault("DBT_PARTIAL_PARSE", "False")
os.environ.setdefault("DBT_VERSION_CHECK", "False")
# Force the pure-Python protobuf implementation as a safety net; the wasm
# protobuf wheel ships with the C accelerator but we don't want to depend on
# its availability.
os.environ.setdefault("PROTOCOL_BUFFERS_PYTHON_IMPLEMENTATION", "python")

# Pre-import dbt modules that touch \`js\` / \`pyodide_js\` BEFORE the sandbox
# importer engages. Once cached in sys.modules, subsequent imports from
# agent code return the cached objects without re-triggering the blocked
# \`js\` import. This is safe because the bootstrap runs in the trusted
# preload phase before any agent code can execute.
#
# Best-effort: any failures here are tolerated because not every dbt build
# uses these modules. The failure surfaces when agent code actually tries
# to use the unimported piece.
_DBT_PREIMPORTS = (
    "dbt.fetch",                  # uses js for HTTP — blocked by sandbox if not pre-loaded
    "dbt.version",                # imports dbt.fetch at top
    "dbt.cli.main",               # transitively imports dbt.version
    "dbt_common.utils.executor",  # caches ThreadPoolExecutor — patched below
)
for _modname in _DBT_PREIMPORTS:
    try:
        __import__(_modname)
    except Exception as _err:  # noqa: BLE001
        # Not fatal — dbt is opt-in via python.dbt and not every wheel set
        # ships the same modules. Surface a warning so debugging is easier.
        print(
            f"warning: dbt-bootstrap could not pre-import {_modname}: {_err!r}",
            flush=True,
        )

del _DBT_PREIMPORTS

# Force dbt_common's MultiThreadedExecutor to be our sync executor, since
# it was imported before our concurrent.futures patch took effect (the
# \`from concurrent.futures import ThreadPoolExecutor\` alias was bound at
# import time). Patching the class directly in dbt_common.utils.executor
# ensures dbt's pool work runs synchronously in our single-threaded VM.
try:
    import dbt_common.utils.executor as _dbt_exec
    if hasattr(_dbt_exec, "MultiThreadedExecutor"):
        _dbt_exec.MultiThreadedExecutor = _SyncThreadPoolExecutor  # type: ignore[attr-defined]
    if hasattr(_dbt_exec, "executor"):
        _orig_executor_factory = _dbt_exec.executor

        def _sync_executor_factory(config):
            return _SyncThreadPoolExecutor()

        _dbt_exec.executor = _sync_executor_factory
except Exception as _err:  # noqa: BLE001
    print(
        f"warning: dbt-bootstrap could not patch dbt_common executor: {_err!r}",
        flush=True,
    )


# Replace dbt.graph.thread_pool.DbtThreadPool with a sync pool stand-in.
# DbtThreadPool subclasses multiprocessing.pool.ThreadPool which spawns
# worker threads in __init__ unconditionally — even when --single-threaded
# is set, dbt's GraphRunnableTask.execute_nodes() instantiates the pool
# before checking the flag, and Pyodide raises "can't start new thread".
class _SyncDbtPool:
    """ThreadPool stand-in: every apply_async runs in-line."""

    def __init__(self, *args, **kwargs):
        pass

    def apply_async(self, func, args=(), kwds=None, callback=None, error_callback=None):
        if kwds is None:
            kwds = {}
        try:
            result = func(*args, **kwds)
            if callback is not None:
                try:
                    callback(result)
                except Exception:
                    pass
            return _SyncFuture(result=result)
        except BaseException as e:
            if error_callback is not None:
                try:
                    error_callback(e)
                except Exception:
                    pass
            return _SyncFuture(exception=e)

    def imap_unordered(self, func, iterable):
        for item in iterable:
            yield func(item)

    def map(self, func, iterable):
        return [func(x) for x in iterable]

    def close(self):
        pass

    def join(self):
        pass

    def terminate(self):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


try:
    import dbt.graph.thread_pool as _dbt_pool
    _dbt_pool.DbtThreadPool = _SyncDbtPool  # type: ignore[attr-defined]
except Exception as _err:  # noqa: BLE001
    print(
        f"warning: dbt-bootstrap could not patch DbtThreadPool: {_err!r}",
        flush=True,
    )

# Also patch the symbol where it's used in dbt.task.runnable
try:
    import dbt.task.runnable as _dbt_runnable
    _dbt_runnable.DbtThreadPool = _SyncDbtPool  # type: ignore[attr-defined]
except Exception:
    pass
`;

/** Default profiles directory and project root used by AgentOs for dbt. */
export const DBT_DEFAULT_PROFILES_DIR = "/root/.dbt";
export const DBT_DEFAULT_PROJECTS_DIR = "/root/dbt-projects";

/** Recommended env bag the AgentOs layer applies when `python.dbt: true`. */
export const DBT_ENV: Record<string, string> = {
  DBT_SEND_ANONYMOUS_USAGE_STATS: "False",
  DBT_SINGLE_THREADED: "True",
  DBT_STATIC_PARSER: "False",
  DBT_USE_EXPERIMENTAL_PARSER: "False",
  DBT_PARTIAL_PARSE: "False",
  DBT_VERSION_CHECK: "False",
  DBT_PROFILES_DIR: DBT_DEFAULT_PROFILES_DIR,
  PROTOCOL_BUFFERS_PYTHON_IMPLEMENTATION: "python",
};
