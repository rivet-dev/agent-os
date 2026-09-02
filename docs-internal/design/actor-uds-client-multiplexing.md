# Actor UDS Client Multiplexing

Status: proposed

Audience: agentOS runtime, native sidecar, actor UDS client, VM SQLite, and
RivetKit integration owners

## 1. Decision

agentOS will multiplex actor SQLite requests over one physical Actor Runtime
Socket connection. A request will hold admission capacity while it is in
flight, but it will not hold the connection, writer, or another request's
response path while awaiting its response.

The connection will have persistent, independently progressing read and write
paths. Each request will be assigned a connection-local `requestId`, registered
in a bounded pending-response table, written as one complete frame, and resolved
only by the response carrying the same `requestId`.

The actor UDS client will keep one physical connection per VM SQLite backend.
It will not use a connection pool because RivetKit transaction leases are scoped
to the connection on which `BEGIN` was received. Every leased statement and its
terminal `COMMIT` or `ROLLBACK` must use that same connection generation.

The transaction-wide `AsyncMutex` currently proposed in
`ActorUdsVmSqliteDatabase` is a stopgap and is not part of the target design.
RivetKit already coordinates leased and unleased database work. agentOS must
allow the later leased requests to reach that coordinator while unrelated
requests remain pending there.

## 2. Incident and root cause

The observed request path produced two approximately 30-second actor SQLite
transactions around a filesystem read. SQL execution and storage round trips
were not responsible for the duration.

The current Rust client stores one `UnixStream` inside
`Mutex<Option<Connection>>`. `request_inner` holds that mutex while it writes a
request and waits for the corresponding response. The higher-level transaction
API performs `BEGIN`, statements, and `COMMIT` as separate calls, releasing the
client mutex between calls.

This permits the following cycle:

```text
agentOS                                      RivetKit actor

BEGIN lease=A ----------------------------> opens transaction A
statement lease=A ------------------------> executes in A; responds
ordinary query B -------------------------> waits behind active A
  holds client connection mutex

COMMIT lease=A
  waits for client connection mutex
```

Query B waits for transaction A to terminate. Transaction A cannot send its
terminal request because B holds the client mutex while waiting for its
response. The client's 30-second timeout eventually breaks the cycle.

The mutex is an implementation artifact, not a protocol requirement. Actor
Runtime Socket v1 carries a `requestId` in every request and response. The
RivetKit server splits the socket, reads frames continuously, spawns request
work independently, and correlates responses by ID. Responses can complete out
of order. The current agentOS client assigns request IDs but only accepts the
next response as the response to the one request holding the stream mutex.

## 3. Goals

1. Allow multiple logical requests to be in flight on one Actor Runtime Socket
   connection.
2. Route every response directly to its registered request waiter by
   `requestId`.
3. Let RivetKit park an unleased request while later leased statements and the
   terminal transaction request continue over the same connection.
4. Bound pending request count, queued request bytes, response staging, task
   count, and connection recovery work.
5. Preserve one connection generation for the full lifetime of every lease.
6. Fail transport and protocol faults deterministically without retrying SQL
   that may already have executed.
7. Run all connection tasks through the VM's injected process-owned
   `RuntimeContext`. Do not create another runtime or unmanaged task topology.
8. Preserve the existing public query, exec, begin, commit, rollback, result,
   and actor-protocol behavior.

## 4. Non-goals

- Changing Actor Runtime Socket v1 or its RivetKit server implementation.
- Adding a pool of UDS connections.
- Retrying an admitted SQL request after an ambiguous transport failure.
- Providing parallel SQLite execution. RivetKit remains the authority for
  transaction and database scheduling.
- Moving transaction ownership into agentOS.
- Raising the 32 MiB protocol frame limit.

## 5. Protocol and ownership invariants

The implementation must preserve these invariants:

1. A request ID is unique among all unanswered requests in one connection
   generation.
2. A response resolves only the waiter registered for its response ID.
3. An unknown, duplicate, malformed, or generation-stale response is a protocol
   failure for that connection generation. It is never delivered to another
   waiter.
4. Frame bytes from separate requests never interleave.
5. The read path continues while any response is pending. The write path never
   waits for a response.
6. `BEGIN`, leased statements, and `COMMIT` or `ROLLBACK` use one connection
   generation.
7. Reconnection creates a new generation. No lease, pending response, request
   ID, or buffered frame crosses generations.
8. A request accepted by a generation is never transparently replayed.
9. Closing a generation produces exactly one terminal result for every pending
   request and releases every admission charge.
10. No mutex is held across both a request write and its response wait.
11. No task repeatedly cancels a partially completed frame read. A reader owns
    its read half and completes or fails each frame before beginning another.

## 6. Target architecture

```text
ActorUdsClient callers
        |
        | bounded request commands
        v
+---------------- connection generation ----------------+
|                                                         |
| connection driver                                       |
|   - assigns request IDs                                  |
|   - owns pending-response state                          |
|   - owns deadlines and terminal transition              |
|   - routes decoded responses to oneshot waiters          |
|       |                                      ^          |
|       | bounded outbound requests            | bounded  |
|       v                                      | events   |
| persistent writer                      persistent reader |
|       |                                      ^          |
+-------|--------------------------------------|-----------+
        v                                      |
              one full-duplex Unix stream
```

The driver, reader, and writer are one bounded connection subsystem. They are
not one task per request. The exact implementation may use two I/O pumps plus a
driver or a cancellation-safe framed transport, but it must preserve the
ownership and progress invariants above.

A simple `tokio::select!` loop around `read_exact` and command receipt is not
acceptable if selecting the command branch cancels a partially completed frame
read. Use a persistent reader task or a codec that retains partial frame state
across polls.

### 6.1 Connection driver

The driver owns all mutable generation state:

- monotonically allocated request IDs;
- `requestId -> PendingRequest` entries;
- request state (`DriverOwned`, `HandedToWriter`, `Writing`, or `Written`);
- the generation deadline queue;
- terminal status and terminal error;
- admission count and queued-byte charges;
- bounded command and response-event receivers;
- the cancellation signal shared with the I/O pumps.

Its request channel is generation-specific. Once the API accepts a command into
that channel, the command belongs to that generation even if the driver has not
yet moved it into the pending table. Generation failure drains and fails those
queued commands; it never silently moves them to a replacement connection.

The driver must not execute SQL work, await an individual response, perform
blocking work, or call user-controlled code. Response delivery through a
oneshot sender occurs after the pending entry has been removed.

The outbound writer channel has the same request-count capacity as the global
in-flight admission limit. Because every request in that channel and every
command being admitted owns one in-flight permit, `try_send` cannot report full
for a valid admitted command. A closed writer channel is a terminal generation
failure. The driver never awaits capacity from the writer while response events
are ready.

### 6.2 Writer

The writer exclusively owns the `OwnedWriteHalf`. It receives a structured
request, its driver-assigned ID, and its queued-byte charge through a bounded
channel. It serializes and writes one frame at a time. Serializing in the single
writer avoids one temporary maximum-sized encoded allocation per concurrent
caller. The connection subsystem therefore retains at most the configured
queued structured-request bytes plus one maximum-sized encoding buffer.

The writer reports `Writing(requestId)` before its first write poll and
`Written(requestId)` after the complete frame has been flushed. The queued-byte
charge travels with the outbound item and is released directly by the writer
after complete write or when cancellation drops the item. Once a request has
been handed to the writer, the driver treats it as potentially written even if
no `Writing` event has arrived, because it cannot safely retract a channel item
racing the writer.

The reader can deliver a fast server response before the driver's separate
writer-event channel delivers `Writing` or `Written`. The response still
completes and removes the pending entry. A later writer event for that ID is
stale local bookkeeping and is ignored; unlike an unknown server response, it
is not a protocol failure. The writer-owned byte charge has exactly-once RAII
release and does not depend on pending-entry presence.

The writer reports I/O failures to the driver. It does not reconnect or retry a
frame. A partially written request is an ambiguous operation, so the entire
generation fails.

### 6.3 Reader

The reader exclusively owns the `OwnedReadHalf`. It continuously reads one
length-delimited frame, enforces the negotiated frame limit before allocation,
decodes the `ServerFrame`, and sends a response event to the driver.

The reader-to-driver event queue should have capacity one. Routing is constant
work, and a one-entry queue bounds decoded response staging while preserving
socket backpressure. The driver gives response events priority over additional
request admission so a hot producer cannot starve completions.

The reader reports EOF, malformed frames, oversized frames, and `GoAway` as
terminal generation events.

### 6.4 Runtime ownership

`ActorUdsClient` will receive the VM-scoped `agentos_runtime::RuntimeContext`.
Connection tasks will be admitted through its `TaskSupervisor` on the one
process-owned Tokio runtime. The driver explicitly selects on
`RuntimeContext::admission_closed()` because closing admission does not cancel
already running tasks automatically. No raw production `tokio::spawn`,
per-connection runtime, OS thread, or per-request task is introduced.

Expected EOF, I/O, timeout, and protocol failures are values reported to the
driver. Reader and writer futures exit with `()` after reporting them. They must
not return expected transport errors through a supervised task API that would
latch a fatal VM task failure. A pump panic, unexplained task disappearance, or
failure to report its terminal state remains a supervised task failure.

## 7. Request lifecycle

### 7.1 Admission

Before a request enters a generation, the client:

1. Reserves one in-flight request slot before creating any internal encoded
   copy.
2. Computes a checked conservative byte charge from the structured request's
   strings, blobs, parameter count, and maximum framing overhead.
3. Reserves that queued-request-byte charge.
4. Creates a response oneshot and moves the structured request and both
   reservations into a bounded, generation-specific driver command.

Admission is nonblocking. Exhaustion returns a typed limit error immediately;
it does not create an unbounded waiter queue. Failed admission releases all
partial reservations and does not allocate a request ID.

The SQLite VM limits gain:

- `limits.sqlite.maxInFlightRequests`, default `64`;
- `limits.sqlite.maxQueuedRequestBytes`, default `67_108_864` (64 MiB).

The in-flight limit covers commands, requests being encoded or written, and
unanswered requests. The queued-byte limit covers a conservative upper bound
for structured requests and their eventual encoded frames until their writes
complete. The size calculation uses checked arithmetic and must never
undercharge the resulting encoded frame. A pending response retains only
bounded metadata after its frame has been written. The writer holds only one
encoding scratch allocation at a time.

The limits must be represented in VM config, native-sidecar limits, TypeScript
client config, Rust client config, generated types, validation, and public
documentation. Limit errors name the exhausted setting and how to raise it.
The client emits a rate-limited warning when either resource reaches 80 percent
of its configured capacity.

`resolve_vm_sqlite` passes the complete validated typed `SqliteLimits` to both
backend construction paths. It does not grow another list of scalar limit
arguments. Limit values are validated and converted to `usize` before channel,
ledger, or buffer allocation.

### 7.2 Registration and write

The driver assigns the next request ID, inserts the pending entry in
`DriverOwned`, and only then hands the structured request to the writer.
Registration-before-write ensures a fast server cannot produce a response
before the waiter exists. The writer adds the assigned ID during serialization,
so ID allocation and complete-frame encoding cannot disagree.

After the handshake, the writer validates the encoded frame against both the
client's absolute limit and the server's negotiated limit before handing any
bytes to `write_frame`. A request larger than either limit fails without being
written and without terminating the otherwise healthy generation.

Request ID wraparound must search for an unused nonzero ID or fail with a typed
exhaustion error. It must never overwrite a pending entry. With at most 64
in-flight requests, a bounded search over occupied IDs is sufficient.

Once the writer completes the frame, the queued-byte charge is released. The
in-flight slot remains charged until response delivery or generation failure.
If actual encoded size ever exceeds its conservative byte charge, the client
treats that as an internal accounting error and fails the request before write.

### 7.3 Response routing

For `ServerFrame::Response`, the driver removes the matching pending entry,
releases its in-flight admission charge, maps the response payload to the
existing typed result or error, and completes that request's oneshot.

Response order has no relationship to request order. Receiving response 12
while requests 10 and 11 remain pending is valid.

### 7.4 Timeouts and caller cancellation

The driver owns one deadline per admitted request and services deadlines with
one bounded timer structure, not one spawned timer task per request.

For the first implementation, expiration of any written request closes that
connection generation and fails every pending request. This is conservative but
preserves the current transport's ambiguity semantics for `BEGIN`, `COMMIT`,
`ROLLBACK`, and partially completed writes. It also prevents unbounded retired
request IDs when a late response may never arrive.

Only a request still exclusively in `DriverOwned` is definitely unwritten and
may time out independently. `HandedToWriter`, `Writing`, and `Written` are all
ambiguous and terminate the generation on deadline. The absence of a
`Writing`/`Written` event is not proof that no bytes reached the socket.

Dropping a caller future does not remove its pending ID immediately. The driver
retains the bounded entry until its response, deadline, or generation closure,
then observes that the response receiver is gone and releases the entry. This
prevents ID reuse while the server may still be processing the request.

An isolated-timeout design may be added later only with bounded retired-ID
tracking and explicit lifecycle-operation semantics. It is not required for
this fix.

## 8. Connection lifecycle

### 8.1 Connect and handshake

Connection creation remains lazy. The first admitted request starts a bounded
connect and protocol-v1 handshake using the existing 10-second connect timeout.
Only a successful handshake publishes a generation to callers.

Requests arriving during connection establishment remain within the same count
and byte admission limits. A failed handshake completes them with the same
typed connection error.

Generation publication is atomic only after the driver and both pumps have been
admitted successfully. If the second or third task cannot be admitted, creation
cancels and joins every task already started, closes the socket, drains all
commands accepted by the unpublished generation, and returns the task-admission
error. No partially constructed generation remains reachable.

### 8.2 Terminal transition

Any of the following terminates the generation:

- reader EOF or read error;
- writer error or partial write;
- malformed, oversized, unknown-ID, or duplicate response;
- server `GoAway`;
- request deadline expiration after a write;
- VM admission closure;
- explicit client shutdown.

Generation termination is idempotent. The first terminal cause is retained for
diagnostics. Teardown proceeds in this order:

1. Mark the generation terminal and close request admission.
2. Close the driver-side event receivers so pumps cannot enqueue more work.
3. Trigger the shared cancellation signal. Pump event sends select on this
   signal and cannot remain blocked on a full event channel.
4. Drain every accepted command still in the driver queue and every pending
   entry. Complete each request exactly once with the typed terminal error and
   release its in-flight charge. Cancellation makes the writer drain or drop
   every outbound item it owns; each item's RAII byte charge releases exactly
   once without the driver taking ownership of the writer's receiver.
5. Await the reader and writer task handles with the configured bounded task
   drain. A stuck pump is aborted and reported through supervision.

Later terminal signals are ignored after their task results have been observed.
Closing a oneshot or command channel is not a substitute for the promised typed
terminal response.

The driver also selects on VM admission closure and on closure of the last
`ActorUdsClient` request sender. Either event performs the same teardown. A new
generation is not published until the previous generation's bounded teardown
has completed, preventing stuck old pumps from accumulating across reconnects.

### 8.3 Reconnection

The client may establish a new generation for a request submitted after the
previous generation is terminal. It does not replay requests accepted by the
old generation. A transaction that observes a generation failure fails as a
whole; its remaining statements and terminal request are not moved to the new
connection.

Lease affinity should be enforced explicitly. The successful `BEGIN` result
records its connection generation alongside the lease key, and leased query,
commit, and rollback calls require that generation. A terminal generation
returns `EndpointClosed` or the retained transport error instead of silently
opening a new connection for the old lease.

The higher-level `ActorUdsVmSqliteDatabase::transaction` continues to generate
one fresh UUID per transaction. Its error and rollback behavior remains the
same except that it uses the generation-bound lease returned by `begin` rather
than passing an unscoped string back into the client.

## 9. API shape

The raw client should make invalid lease routing unrepresentable. Replace the
public internal sequence:

```text
begin(key)
query_with_lease(..., key)
commit(key)
```

with an internal transaction handle:

```text
transaction = client.begin(key, timeout)
transaction.query(...)
transaction.commit()
transaction.rollback()
```

The handle contains the lease key, connection generation, and local
`Active`/`Terminal` state. A definitive `CommitOk` or `RollbackOk` marks it
terminal, as does connection-generation failure. A server-side commit error
that leaves the generation live keeps the handle active so the existing
explicit rollback path remains available. Duplicate terminal attempts fail
locally. Dropping a live handle does not synchronously block. Its server lease
remains bounded by the RivetKit lease timeout, and the caller's existing
explicit rollback paths remain mandatory and observable.

This is an internal Rust API change. The VM SQLite trait and public TypeScript
and Rust agentOS APIs do not expose the handle.

## 10. Transaction progress after multiplexing

The incident ordering is valid and must complete:

```text
write BEGIN A
receive BEGIN A response
write statement A1
receive A1 response
write ordinary query B
RivetKit parks B behind A
write statement A2
receive A2 response
write COMMIT A
receive COMMIT A response
receive query B response
```

agentOS does not need to know that B is waiting behind A. It only needs to keep
the writer available and dispatch each eventual response by ID.

## 11. Error and observability contract

Add or preserve typed errors for:

- local in-flight request exhaustion;
- local queued-request-byte exhaustion;
- connect timeout;
- request timeout;
- connection generation closed;
- unknown or duplicate response ID;
- malformed or oversized frame;
- request ID exhaustion;
- task admission failure;
- server-provided SQLite, queue, lease, and endpoint errors.

No failure is swallowed. The connection's terminal cause is logged once with
its generation, number of failed pending requests, admitted request bytes, and
whether any lease was active. Logs do not include SQL text, parameters, socket
paths, or credentials by default.

Metrics should expose bounded counters and gauges for:

- active connection generations;
- reconnects by cause;
- in-flight requests;
- queued request bytes;
- request latency by operation class;
- out-of-order responses;
- pending requests failed by generation closure;
- local admission rejections.

Request IDs and lease keys are high-cardinality and must not be metric labels.

## 12. Tests

### 12.1 Actor UDS client integration tests

Add deterministic tests in `crates/actor-uds-client/tests/` that prove:

1. Request 2 reaches the server before request 1 receives a response.
2. Responding to request 2 before request 1 resolves the correct caller for
   each response.
3. A third request can be written while two responses remain pending.
4. Concurrent frame writes never interleave bytes.
5. Unknown and duplicate response IDs terminate the generation and fail all
   pending callers.
6. EOF, `GoAway`, malformed frames, oversized frames, and partial writes fail
   every pending caller exactly once.
7. In-flight count and queued-byte limits reject deterministically and recover
   after completions.
8. A timed-out written request terminates the generation without replay, and a
   subsequent request uses a fresh generation.
9. A pure request-ID allocator with an injectable starting value wraps without
   overwriting a pending request. The test does not send billions of requests.
10. Dropped callers do not leak request IDs, admission permits, bytes, or tasks.
11. Repeated connect/fail/reconnect cycles leave no reader or writer tasks.

Use a scripted transport or a test-only transport abstraction for partial-write
state transitions. Unix socket buffer timing is not a deterministic way to
prove `HandedToWriter` and partial-write teardown behavior.

### 12.2 VM SQLite regression

Replace the stopgap serialization test with a server that models RivetKit's
real multiplexing behavior:

1. Start transaction A, accept its first leased statement A1, and withhold the
   A1 response.
2. Start ordinary query B and assert that B reaches the same server connection
   while A1 remains unanswered. This assertion directly distinguishes the
   multiplexed client from the old round-trip mutex.
3. Keep B's response parked, then release A1's response.
4. Assert that A's next statement and `COMMIT` arrive on the same connection
   while B remains unanswered.
5. Respond to the commit, then respond to B.
6. Assert that the transaction and ordinary query both complete without a
   timeout.

The fake server stores B as pending and continues reading later frames. It must
not await B inline in its connection read loop. Use notifications or barriers
on a current-thread runtime and no scheduling sleeps.

The test must fail on agentOS 0.2.18/current main because the old client cannot
send the commit while B owns its round-trip mutex. It must not pass merely
because agentOS serializes B until after commit.

### 12.3 Soak and resource tests

Add a focused ignored soak test for high request counts and a default fast test
that repeatedly pipelines the configured maximum. Assert the client's internal
task, pending-entry, request-count, and byte-accounting snapshots rather than
process RSS. Also assert correct response routing and no request ID collision.
The saturation test must use small configured limits and remain in the default
suite.

## 13. Implementation sequence

1. Add client limits and thread them through VM configuration, TypeScript and
   Rust clients, generated types, defaults, validation, and documentation.
2. Inject the VM-scoped `RuntimeContext` into `ActorUdsClient` construction.
3. Implement one connection generation with bounded driver commands,
   persistent reader and writer ownership, pending-response routing, and
   idempotent terminal cleanup.
4. Add the generation-bound transaction handle and migrate VM SQLite calls.
5. Add direct multiplexer, failure, bounds, and lifecycle tests.
6. Replace the VM SQLite stopgap regression with the real interleaving test.
7. Remove the outer `ActorUdsVmSqliteDatabase` operation mutex.
8. Run focused actor UDS and VM SQLite integration tests, native-sidecar type
   checks, generated-type parity checks, and the relevant architecture guards.

Each intermediate commit must compile, but the final PR should present one
coherent implementation rather than retaining the stopgap serialization layer.

## 14. Acceptance criteria

The change is complete when:

- no client mutex spans request write and response wait;
- two requests can be concurrently in flight on one UDS connection;
- responses route correctly in either order;
- a parked unleased query cannot prevent a later leased commit from being
  written;
- every lease is bound to one live connection generation;
- transport failure never retries an ambiguous SQL request;
- all queues, byte buffers, pending maps, timers, and tasks are bounded and
  observable;
- generation teardown fails and releases all pending state exactly once;
- all production tasks are supervised by the injected runtime;
- the stopgap transaction-wide operation lock is absent;
- focused tests reproduce the original deadlock on the old client and pass on
  the multiplexed client.

## 15. Rejected alternatives

### Serialize complete VM SQLite operations

This prevents the deadlock but disables protocol multiplexing, adds
head-of-line blocking in agentOS, and hides the incorrect client response model.

### Open one connection per request

RivetKit leases are connection-scoped, so transaction statements would require
connection affinity. A pool adds routing and lifecycle complexity without
fixing the existing protocol implementation.

### Use a writer mutex and let every caller read

Multiple independent readers cannot safely determine which task should consume
the next response. One persistent reader must dispatch by request ID.

### Hold separate read and write mutexes in caller tasks

A read mutex still serializes response ownership and can assign an out-of-order
response to the wrong caller. Correlation belongs in a pending-response table.

### Retry after reconnect

The server may have executed a query or committed a transaction before the
transport failed. Automatic replay can duplicate writes or report an incorrect
transaction outcome.

## 16. Resolved design decisions

### 16.1 Timeout blast radius

The first implementation terminates the entire connection generation
when any written request reaches its deadline. This matches the current
client’s conservative ambiguity behavior and keeps retired request IDs bounded,
but one slow independent query will fail otherwise healthy concurrent requests.

Per-request timeout isolation is deferred. Adding it later requires bounded
retired-ID tracking, a defined tombstone lifetime for late responses, and
separate timeout semantics for ordinary queries and transaction lifecycle
operations.

### 16.2 Dropped transaction cleanup

The implementation passes an explicit finite lease timeout equal to the
configured agentOS request timeout, keeps explicit rollback on normal error
paths, and does not spawn rollback work from `Drop`. A destructor cannot await
rollback, while spawn-on-drop would add separate task-admission and failure
semantics.
