#include "v8-platform.h"
#include "v8-sandbox.h"

extern "C" void agentos_v8_initialize_sandbox_hardware_before_thread_creation() {
  v8::SandboxHardwareSupport::InitializeBeforeThreadCreation();
}

extern "C" void agentos_v8_set_default_thread_isolation_permissions() {
  v8::ThreadIsolatedAllocator::SetDefaultPermissionsForSignalHandler();
}
