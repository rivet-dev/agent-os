//! Runtime telemetry adapter for the kernel-owned resource ledger.

pub use agentos_resource::{
    LimitError, Reservation, ResourceClass, ResourceLedger, ResourceLimit, ResourceUsage,
    ResourceUsageObserver, SharedReservation,
};

use crate::metrics::{BufferMetricClass, ResourceMetricClass, RuntimeMetrics};

impl ResourceUsageObserver for RuntimeMetrics {
    fn observe_usage(&self, resource: ResourceClass, used: usize) {
        match resource {
            ResourceClass::Capabilities => {
                self.observe_resource(ResourceMetricClass::Capabilities, used)
            }
            ResourceClass::ReadyHandles => {
                self.observe_resource(ResourceMetricClass::ReadyHandles, used)
            }
            ResourceClass::Sockets => self.observe_resource(ResourceMetricClass::Sockets, used),
            ResourceClass::Connections => {
                self.observe_resource(ResourceMetricClass::Connections, used)
            }
            ResourceClass::BufferedBytes | ResourceClass::HandleCommandBytes => {
                self.observe_buffer(BufferMetricClass::Native, used)
            }
            ResourceClass::Datagrams | ResourceClass::UdpDatagrams => {
                self.observe_resource(ResourceMetricClass::Datagrams, used)
            }
            ResourceClass::HandleCommands => {
                self.observe_resource(ResourceMetricClass::HandleCommands, used)
            }
            ResourceClass::BridgeCalls => {
                self.observe_resource(ResourceMetricClass::BridgeCalls, used)
            }
            ResourceClass::BridgeRequestBytes | ResourceClass::BridgeResponseBytes => {
                self.observe_buffer(BufferMetricClass::Bridge, used)
            }
            ResourceClass::AsyncCompletions => {
                self.observe_resource(ResourceMetricClass::AsyncCompletions, used)
            }
            ResourceClass::AsyncCompletionBytes => {
                self.observe_buffer(BufferMetricClass::Bridge, used)
            }
            ResourceClass::UdpBytes => self.observe_buffer(BufferMetricClass::Datagram, used),
            ResourceClass::TlsBytes => self.observe_buffer(BufferMetricClass::Tls, used),
            ResourceClass::Timers => self.observe_resource(ResourceMetricClass::Timers, used),
            ResourceClass::Tasks => self.observe_resource(ResourceMetricClass::Tasks, used),
            ResourceClass::ExecutorSlots => {}
            ResourceClass::ExecutorBytes => self.observe_buffer(BufferMetricClass::Executor, used),
            ResourceClass::WasmMemoryBytes => {
                self.observe_buffer(BufferMetricClass::Executor, used)
            }
            ResourceClass::WasmThreads => {}
            ResourceClass::Http2BufferedBytes => {
                self.observe_buffer(BufferMetricClass::Http2, used)
            }
            ResourceClass::Http2Connections => {
                self.observe_resource(ResourceMetricClass::Http2Connections, used)
            }
            ResourceClass::Http2Streams => {
                self.observe_resource(ResourceMetricClass::Http2Streams, used)
            }
            ResourceClass::Http2HeaderBytes
            | ResourceClass::Http2DataBytes
            | ResourceClass::Http2Commands
            | ResourceClass::Http2CommandBytes
            | ResourceClass::Http2Events
            | ResourceClass::Http2EventBytes => {}
        }
    }
}
