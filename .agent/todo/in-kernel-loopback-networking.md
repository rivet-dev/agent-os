# In-kernel loopback networking

Currently, loopback connections between guest processes go through real host TCP sockets with port translation (guest port 3000 → random host port 49152). The sidecar manages a NAT-like layer to make this transparent.

The old JS kernel had pure in-memory loopback — connections between processes in the same VM stayed entirely in-kernel with no real host sockets. This is cleaner and more correct:

- No host port exhaustion under heavy use
- No real TCP overhead for intra-VM traffic
- Eliminates the port translation layer and `ActiveTcpListener` bookkeeping in the sidecar
- Better isolation — loopback traffic never touches the host network stack
- Matches the virtualization invariant that guest I/O should go through the kernel

## What it would take

- Kernel socket table needs real data transport (buffered byte channels between socket pairs), not just a registry
- Loopback `connect()` checks the kernel socket table for a matching listener, creates an in-kernel pipe pair
- External connections still delegate to `HostNetworkAdapter` with real sockets
- `vm.fetch()` from the host side would need a special path to inject into the kernel socket table
- TCP semantics (backpressure, half-close, SO_REUSEADDR) need at least minimal emulation

## Why not now

The current real-socket approach works and gives correct TCP semantics for free. This is a significant effort and lower priority than completing the TS→Rust migration.
