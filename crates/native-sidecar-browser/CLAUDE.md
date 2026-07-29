# Browser Support

- Browser support is untested after the agentos split; only build-level validation is required during migration.
- Provenance: moved from rivet-dev/agentos@87ed8e21e454.
- Keep the browser sidecar separate from the native sidecar because worker transport and main-thread ownership differ from stdio/socket transport.
