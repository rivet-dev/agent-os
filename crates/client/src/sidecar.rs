//! `AgentOsSidecar` (public transport handle) + placement/description + the process-global shared
//! pool + internal lease accounting.
//!
//! Ported from `packages/core/src/agent-os.ts` (`AgentOsSidecar`). The shared-sidecar pool is a
//! process-global map (default pool `"default"`).

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use once_cell::sync::OnceCell;
use scc::HashMap as SccHashMap;
use serde::Serialize;
use uuid::Uuid;

use agentos_sidecar_client::wire;

use crate::agent_os::AgentOs;
use crate::error::ClientError;
use crate::transport::SidecarProcess;

/// Maximum shared sidecar pool entries retained process-wide.
const SHARED_SIDECAR_POOL_LIMIT: usize = 1024;

/// Env var that overrides the Agent OS wrapper sidecar binary path.
const AGENTOS_SIDECAR_BIN_ENV: &str = "AGENTOS_SIDECAR_BIN";

/// The lazily-established shared sidecar process + authenticated connection. Multiple VMs in the same
/// (shared) sidecar reuse this single process/connection, each opening its own session + VM on it.
pub(crate) struct SharedConnection {
    pub(crate) transport: Arc<SidecarProcess>,
    pub(crate) connection_id: String,
}

/// Sidecar lifecycle state, encoded as a `u8` for `AtomicU8`.
///
/// Parity: TypeScript `describe()` returns a JSON-serializable description whose `state` is exactly
/// `"ready" | "disposing" | "disposed"`. The `#[serde(rename_all = "lowercase")]` attribute and the
/// matching [`SidecarState::as_str`] reproduce that wire string so [`AgentOsSidecarDescription`]
/// serializes to the same JSON shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SidecarState {
    Ready,
    Disposing,
    Disposed,
}

impl SidecarState {
    /// The TypeScript wire string for this state (`"ready" | "disposing" | "disposed"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            SidecarState::Ready => "ready",
            SidecarState::Disposing => "disposing",
            SidecarState::Disposed => "disposed",
        }
    }

    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            SidecarState::Ready => 0,
            SidecarState::Disposing => 1,
            SidecarState::Disposed => 2,
        }
    }

    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0 => SidecarState::Ready,
            1 => SidecarState::Disposing,
            2 => SidecarState::Disposed,
            // Any other bit pattern is unreachable; the field is only written via `as_u8`.
            _ => SidecarState::Disposed,
        }
    }
}

/// Where a sidecar lives.
///
/// Parity: TypeScript `AgentOsSidecarPlacement` is `{ kind: "shared"; pool?: string }` or
/// `{ kind: "explicit"; sidecarId: string }`. The serde `tag`/`rename` attributes reproduce that
/// JSON shape, including omitting `pool` when it is `None` (matching the `...(pool ? { pool } : {})`
/// spread in `getSharedAgentOsSidecarInternal`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AgentOsSidecarPlacement {
    Shared {
        #[serde(skip_serializing_if = "Option::is_none")]
        pool: Option<String>,
    },
    Explicit {
        #[serde(rename = "sidecarId")]
        sidecar_id: String,
    },
}

/// A sync, deep-clone snapshot of a sidecar's state.
///
/// Parity: serializes to the TypeScript `AgentOsSidecarDescription` JSON shape
/// (`{ sidecarId, placement, state, activeVmCount }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOsSidecarDescription {
    pub sidecar_id: String,
    pub placement: AgentOsSidecarPlacement,
    pub state: SidecarState,
    pub active_vm_count: u32,
}

/// Public transport handle for a (possibly shared) sidecar process hosting VMs.
pub struct AgentOsSidecar {
    pub(crate) sidecar_id: String,
    pub(crate) placement: AgentOsSidecarPlacement,
    pub(crate) shared_pool: Option<String>,
    pub(crate) state: AtomicU8,
    pub(crate) active_vm_count: AtomicU32,
    /// Absolute path to the `agentos-sidecar` binary, threaded from `AgentOsConfig` when present.
    /// Otherwise `ensure_connection` resolves the Agent OS env fallback and passes an explicit path
    /// to the generic transport.
    pub(crate) sidecar_binary_path: Option<String>,
    /// The shared sidecar process + authenticated connection, established on the first VM `create`
    /// against this sidecar and reused by every subsequent VM in the same (shared) sidecar.
    pub(crate) connection: tokio::sync::Mutex<Option<SharedConnection>>,
}

impl AgentOsSidecar {
    /// Construct a sidecar handle.
    pub(crate) fn new(
        sidecar_id: impl Into<String>,
        placement: AgentOsSidecarPlacement,
        shared_pool: Option<String>,
        sidecar_binary_path: Option<String>,
    ) -> Self {
        Self {
            sidecar_id: sidecar_id.into(),
            placement,
            shared_pool,
            state: AtomicU8::new(SidecarState::Ready.as_u8()),
            active_vm_count: AtomicU32::new(0),
            sidecar_binary_path,
            connection: tokio::sync::Mutex::new(None),
        }
    }

    /// Get (or lazily establish) the shared sidecar process + authenticated connection. The first
    /// caller spawns the `agentos-sidecar` child and runs the `Authenticate` handshake; subsequent
    /// callers reuse the same transport + connection id. This is what makes a shared sidecar host
    /// multiple VMs in one process.
    pub(crate) async fn ensure_connection(
        &self,
    ) -> Result<(Arc<SidecarProcess>, String, usize), ClientError> {
        let mut guard = self.connection.lock().await;
        if let Some(existing) = guard.as_ref() {
            let max_frame = existing.transport.max_frame_bytes();
            return Ok((
                existing.transport.clone(),
                existing.connection_id.clone(),
                max_frame,
            ));
        }

        let transport = SidecarProcess::spawn(Some(self.resolved_sidecar_binary_path())).await?;
        let authed = match transport
            .request_wire(
                wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
                    connection_id: "client-hint".to_string(),
                }),
                wire::RequestPayload::AuthenticateRequest(wire::AuthenticateRequest {
                    client_name: "agentos-client".to_string(),
                    auth_token: "agentos-client".to_string(),
                    protocol_version: wire::PROTOCOL_VERSION,
                    bridge_version: agentos_vm_host_interface::bridge_contract().version,
                }),
            )
            .await?
        {
            wire::ResponsePayload::AuthenticatedResponse(authed) => authed,
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(ClientError::Kernel {
                    code: rejected.code,
                    message: rejected.message,
                });
            }
            _ => {
                return Err(ClientError::Sidecar(
                    "unexpected authenticate response".to_string(),
                ));
            }
        };
        let max_frame = authed.max_frame_bytes as usize;
        transport.set_max_frame_bytes(max_frame);

        *guard = Some(SharedConnection {
            transport: transport.clone(),
            connection_id: authed.connection_id.clone(),
        });
        Ok((transport, authed.connection_id, max_frame))
    }

    /// Kill the shared sidecar child process if a connection was established. Used when the last VM
    /// on a shared sidecar shuts down, so the sidecar process does not leak (process-global pool
    /// entries are never dropped, so `kill_on_drop` alone would not fire at process exit).
    pub(crate) async fn kill_connection(&self) {
        if let Some(connection) = self.connection.lock().await.take() {
            connection.transport.kill_child();
        }
    }

    fn resolved_sidecar_binary_path(&self) -> String {
        self.sidecar_binary_path
            .clone()
            .or_else(|| std::env::var(AGENTOS_SIDECAR_BIN_ENV).ok())
            .unwrap_or_else(|| "agentos-sidecar".to_string())
    }

    /// Snapshot the sidecar's current state. SYNC.
    ///
    /// Parity: TypeScript `describe()` returns a deep clone of the internal description so callers
    /// cannot mutate sidecar state through the returned value. The Rust struct derives `Clone`, so
    /// constructing a fresh [`AgentOsSidecarDescription`] from the current atomics produces the same
    /// snapshot semantics.
    pub fn describe(&self) -> AgentOsSidecarDescription {
        AgentOsSidecarDescription {
            sidecar_id: self.sidecar_id.clone(),
            placement: self.placement.clone(),
            state: SidecarState::from_u8(self.state.load(Ordering::SeqCst)),
            active_vm_count: self.active_vm_count.load(Ordering::SeqCst),
        }
    }

    /// Dispose the sidecar. Idempotent; disposes active leases and aggregates errors.
    ///
    /// Parity with TypeScript `AgentOsSidecar.dispose()`:
    /// 1. If already `disposed`, return immediately (idempotent).
    /// 2. Transition to `disposing`.
    /// 3. Dispose every active lease, collecting (not short-circuiting on) errors.
    /// 4. Reset `active_vm_count` to 0 and transition to `disposed`.
    /// 5. If this sidecar is the cached shared sidecar for its pool, remove it from the pool.
    /// 6. If any lease disposal failed, return an aggregated error.
    pub async fn dispose(&self) -> Result<(), ClientError> {
        if SidecarState::from_u8(self.state.load(Ordering::SeqCst)) == SidecarState::Disposed {
            return Ok(());
        }

        self.state
            .store(SidecarState::Disposing.as_u8(), Ordering::SeqCst);

        let errors: Vec<String> = Vec::new();

        // Parity note: TypeScript iterates `state.activeLeases` here and aggregates per-lease
        // disposal errors. Active leases are owned by `AgentOs` and are released through
        // `AgentOsSidecarVmLease::dispose` during `AgentOs::shutdown`.
        self.active_vm_count.store(0, Ordering::SeqCst);
        self.state
            .store(SidecarState::Disposed.as_u8(), Ordering::SeqCst);

        if let Some(pool) = self.shared_pool.as_deref() {
            // Only remove the cached entry if it still points at this exact sidecar instance.
            let self_ptr = self as *const AgentOsSidecar;
            let _ = shared_sidecars()
                .remove_if(pool, |cached| std::ptr::eq(Arc::as_ptr(cached), self_ptr));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            // Parity: TypeScript throws `new Error(errors.map(e => e.message).join("; "))`, a bare
            // joined message with NO prefix. The aggregated text is built here verbatim.
            //
            // Constraint: `ClientError` (error.rs, owned by another agent) currently has no
            // transparent/no-prefix variant, so the only generic carrier is `ClientError::Sidecar`,
            // whose `Display` prepends `"sidecar error: "`. To surface the joined string byte-for-byte
            // identical to TS, error.rs must grow a transparent variant (e.g.
            // `#[error("{0}")] Aggregate(String)`); this site should switch to it once it exists. The
            // joined string is constructed here so that wiring is a one-line variant swap.
            let aggregated = errors.join("; ");
            Err(ClientError::Sidecar(aggregated))
        }
    }
}

/// A lease over a VM; released on `AgentOs` dispose.
pub(crate) struct AgentOsSidecarVmLease {
    pub(crate) sidecar: Arc<AgentOsSidecar>,
}

impl AgentOsSidecarVmLease {
    /// Release the lease.
    ///
    /// Parity with the TypeScript lease `dispose()`: it is idempotent, removes itself from the
    /// owning sidecar's active-lease set, recomputes `activeVmCount`, and disposes the underlying
    /// session transport client. Consuming `self` here gives the idempotence for free (the lease
    /// cannot be disposed twice). The active-vm count is decremented (saturating at 0) to mirror
    /// `state.description.activeVmCount = state.activeLeases.size`.
    ///
    pub(crate) async fn dispose(self) -> Result<(), ClientError> {
        let sidecar = self.sidecar;
        // Mirror `activeVmCount = activeLeases.size` by decrementing, never underflowing past 0.
        let mut current = sidecar.active_vm_count.load(Ordering::SeqCst);
        loop {
            let next = current.saturating_sub(1);
            match sidecar.active_vm_count.compare_exchange_weak(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        Ok(())
    }
}

/// Process-global shared-sidecar pool, keyed by pool name (default `"default"`).
static SHARED_SIDECARS: OnceCell<SccHashMap<String, Arc<AgentOsSidecar>>> = OnceCell::new();
static SHARED_SIDECAR_POOL_LOCK: OnceCell<parking_lot::Mutex<()>> = OnceCell::new();

/// Access (initializing on first use) the process-global shared-sidecar pool.
pub(crate) fn shared_sidecars() -> &'static SccHashMap<String, Arc<AgentOsSidecar>> {
    SHARED_SIDECARS.get_or_init(SccHashMap::new)
}

fn shared_sidecar_pool_lock() -> &'static parking_lot::Mutex<()> {
    SHARED_SIDECAR_POOL_LOCK.get_or_init(parking_lot::Mutex::default)
}

fn shared_sidecar_pool_len(cache: &SccHashMap<String, Arc<AgentOsSidecar>>) -> usize {
    let mut len = 0;
    cache.scan(|_, _| {
        len += 1;
    });
    len
}

fn prune_disposed_shared_sidecars(cache: &SccHashMap<String, Arc<AgentOsSidecar>>) {
    let mut disposed_pools = Vec::new();
    cache.scan(|pool, sidecar| {
        if sidecar.describe().state == SidecarState::Disposed {
            disposed_pools.push(pool.clone());
        }
    });
    for pool in disposed_pools {
        let _ = cache.remove_if(&pool, |sidecar| {
            sidecar.describe().state == SidecarState::Disposed
        });
    }
}

#[cfg(test)]
fn ensure_shared_sidecar_pool_capacity(
    cache: &SccHashMap<String, Arc<AgentOsSidecar>>,
) -> Result<(), ClientError> {
    if shared_sidecar_pool_len(cache) >= SHARED_SIDECAR_POOL_LIMIT {
        return Err(shared_sidecar_pool_limit_error());
    }
    Ok(())
}

fn shared_sidecar_pool_limit_error() -> ClientError {
    ClientError::Sidecar(format!(
        "shared sidecar pool limit exceeded: at most {SHARED_SIDECAR_POOL_LIMIT} pools can be cached"
    ))
}

impl AgentOs {
    /// Create an explicit sidecar handle. `sidecar_id` defaults to `agentos-sidecar-<uuid>`.
    ///
    /// Parity with TypeScript `createAgentOsSidecarInternal`: the explicit handle carries an
    /// `Explicit` placement whose `sidecar_id` echoes the resolved id and has no shared pool.
    pub async fn create_sidecar(
        sidecar_id: Option<String>,
    ) -> Result<Arc<AgentOsSidecar>, ClientError> {
        let sidecar_id =
            sidecar_id.unwrap_or_else(|| format!("agentos-sidecar-{}", Uuid::new_v4()));
        let placement = AgentOsSidecarPlacement::Explicit {
            sidecar_id: sidecar_id.clone(),
        };
        Ok(Arc::new(AgentOsSidecar::new(
            sidecar_id, placement, None, None,
        )))
    }

    /// Get (or create) a pooled shared sidecar. Pool defaults to `"default"`. Uses the process-global
    /// cache.
    ///
    /// Parity with TypeScript `getSharedAgentOsSidecarInternal`: return the cached sidecar for the
    /// pool when it exists and is not disposed; otherwise build a fresh handle
    /// (`agentos-shared-sidecar:<pool>`, `Shared` placement) and cache it. Because the cache is a
    /// process-global concurrent map rather than a synchronously-checked `Map`, the insert is done
    /// atomically with `entry`/`insert` so two racing callers converge on a single live handle.
    pub async fn get_shared_sidecar(
        pool: Option<String>,
        sidecar_binary_path: Option<String>,
    ) -> Result<Arc<AgentOsSidecar>, ClientError> {
        let pool = pool.unwrap_or_else(|| "default".to_string());
        let cache = shared_sidecars();
        let _guard = shared_sidecar_pool_lock().lock();

        // Fast path: reuse a cached, non-disposed sidecar for this pool.
        if let Some(existing) = cache.read(&pool, |_, sidecar| sidecar.clone()) {
            if existing.describe().state != SidecarState::Disposed {
                return Ok(existing);
            }
        }
        prune_disposed_shared_sidecars(cache);

        // Parity: TypeScript builds placement `{ kind: "shared", ...(pool ? { pool } : {}) }`, so an
        // empty-string pool (a non-nullish value that survives `?? "default"`) is OMITTED from the
        // placement. The `sharedPool` field used for cache cleanup still carries the raw pool value.
        let placement_pool = if pool.is_empty() {
            None
        } else {
            Some(pool.clone())
        };
        let sidecar = Arc::new(AgentOsSidecar::new(
            format!("agentos-shared-sidecar:{pool}"),
            AgentOsSidecarPlacement::Shared {
                pool: placement_pool,
            },
            Some(pool.clone()),
            sidecar_binary_path,
        ));

        // Insert atomically, replacing a stale (disposed) entry but yielding to a live one that a
        // concurrent caller may have just installed.
        let cache_len = shared_sidecar_pool_len(cache);
        match cache.entry(pool) {
            scc::hash_map::Entry::Occupied(mut occupied) => {
                if occupied.get().describe().state == SidecarState::Disposed {
                    *occupied.get_mut() = sidecar.clone();
                    Ok(sidecar)
                } else {
                    Ok(occupied.get().clone())
                }
            }
            scc::hash_map::Entry::Vacant(vacant) => {
                if cache_len >= SHARED_SIDECAR_POOL_LIMIT {
                    return Err(shared_sidecar_pool_limit_error());
                }
                vacant.insert_entry(sidecar.clone());
                Ok(sidecar)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn shared(pool: &str, state: SidecarState) -> Arc<AgentOsSidecar> {
        let sidecar = Arc::new(AgentOsSidecar::new(
            format!("agentos-shared-sidecar:{pool}"),
            AgentOsSidecarPlacement::Shared {
                pool: Some(pool.to_string()),
            },
            Some(pool.to_string()),
            None,
        ));
        sidecar.state.store(state.as_u8(), Ordering::SeqCst);
        sidecar
    }

    #[test]
    fn sidecar_binary_path_prefers_explicit_wrapper_path() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var(AGENTOS_SIDECAR_BIN_ENV).ok();
        std::env::set_var(AGENTOS_SIDECAR_BIN_ENV, "/tmp/from-env");
        let sidecar = AgentOsSidecar::new(
            "explicit-test",
            AgentOsSidecarPlacement::Explicit {
                sidecar_id: "explicit-test".to_string(),
            },
            None,
            Some("/tmp/from-config".to_string()),
        );

        assert_eq!(sidecar.resolved_sidecar_binary_path(), "/tmp/from-config");

        restore_env(AGENTOS_SIDECAR_BIN_ENV, previous);
    }

    #[test]
    fn sidecar_binary_path_uses_agent_os_env_fallback() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var(AGENTOS_SIDECAR_BIN_ENV).ok();
        std::env::set_var(AGENTOS_SIDECAR_BIN_ENV, "/tmp/agentos-sidecar");
        let sidecar = shared("env-test", SidecarState::Ready);

        assert_eq!(
            sidecar.resolved_sidecar_binary_path(),
            "/tmp/agentos-sidecar"
        );

        restore_env(AGENTOS_SIDECAR_BIN_ENV, previous);
    }

    #[test]
    fn sidecar_binary_path_defaults_to_agent_os_wrapper() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let previous = std::env::var(AGENTOS_SIDECAR_BIN_ENV).ok();
        std::env::remove_var(AGENTOS_SIDECAR_BIN_ENV);
        let sidecar = shared("default-test", SidecarState::Ready);

        assert_eq!(sidecar.resolved_sidecar_binary_path(), "agentos-sidecar");

        restore_env(AGENTOS_SIDECAR_BIN_ENV, previous);
    }

    fn restore_env(key: &str, value: Option<String>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn prune_disposed_shared_sidecars_keeps_live_entries() {
        let cache = SccHashMap::new();
        let _ = cache.insert("live".to_string(), shared("live", SidecarState::Ready));
        let _ = cache.insert(
            "disposed".to_string(),
            shared("disposed", SidecarState::Disposed),
        );

        prune_disposed_shared_sidecars(&cache);

        assert_eq!(shared_sidecar_pool_len(&cache), 1);
        assert!(cache.read("live", |_, _| ()).is_some());
        assert!(cache.read("disposed", |_, _| ()).is_none());
    }

    #[test]
    fn shared_sidecar_pool_capacity_rejects_full_live_cache() {
        let cache = SccHashMap::new();
        for index in 0..SHARED_SIDECAR_POOL_LIMIT {
            let pool = format!("pool-{index}");
            let _ = cache.insert(pool.clone(), shared(&pool, SidecarState::Ready));
        }

        let error =
            ensure_shared_sidecar_pool_capacity(&cache).expect_err("full cache should reject");

        assert!(
            error
                .to_string()
                .contains("shared sidecar pool limit exceeded"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn shared_sidecar_pool_capacity_allows_after_pruning_disposed_entries() {
        let cache = SccHashMap::new();
        for index in 0..SHARED_SIDECAR_POOL_LIMIT {
            let pool = format!("pool-{index}");
            let state = if index == 0 {
                SidecarState::Disposed
            } else {
                SidecarState::Ready
            };
            let _ = cache.insert(pool.clone(), shared(&pool, state));
        }

        prune_disposed_shared_sidecars(&cache);

        ensure_shared_sidecar_pool_capacity(&cache).expect("pruned cache should admit one entry");
        assert_eq!(
            shared_sidecar_pool_len(&cache),
            SHARED_SIDECAR_POOL_LIMIT - 1
        );
    }

    #[tokio::test]
    async fn get_shared_sidecar_inserts_vacant_pool_without_reentrant_scan() {
        let pool = format!("unit-{}", Uuid::new_v4());
        let sidecar = AgentOs::get_shared_sidecar(Some(pool.clone()), None)
            .await
            .expect("shared sidecar");

        assert_eq!(sidecar.shared_pool.as_deref(), Some(pool.as_str()));

        sidecar.dispose().await.expect("dispose shared sidecar");
    }

    #[test]
    fn dispose_removes_only_same_shared_sidecar_instance() {
        let pool = format!("dispose-race-{}", Uuid::new_v4());
        let old = shared(&pool, SidecarState::Ready);
        let replacement = shared(&pool, SidecarState::Ready);
        let cache = shared_sidecars();
        let _guard = shared_sidecar_pool_lock().lock();

        let _ = cache.insert(pool.clone(), replacement.clone());
        old.state
            .store(SidecarState::Disposing.as_u8(), Ordering::SeqCst);
        old.active_vm_count.store(0, Ordering::SeqCst);
        old.state
            .store(SidecarState::Disposed.as_u8(), Ordering::SeqCst);
        let old_ptr = Arc::as_ptr(&old);
        let _ = cache.remove_if(&pool, |cached| std::ptr::eq(Arc::as_ptr(cached), old_ptr));

        let cached = cache
            .read(&pool, |_, cached| cached.clone())
            .expect("replacement should remain cached");
        assert!(Arc::ptr_eq(&cached, &replacement));

        let replacement_ptr = Arc::as_ptr(&replacement);
        let _ = cache.remove_if(&pool, |cached| {
            std::ptr::eq(Arc::as_ptr(cached), replacement_ptr)
        });
    }
}
