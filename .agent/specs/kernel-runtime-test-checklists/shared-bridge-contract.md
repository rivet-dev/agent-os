# Shared Bridge Contract Test Checklist

Source files:
- `crates/bridge/src/lib.rs`
- `crates/bridge/bridge-contract.json`

Suggested test homes:
- `crates/bridge/tests/bridge.rs`
- `crates/bridge/tests/contract_parity.rs` (new file if needed)
- `crates/bridge/tests/types.rs` (new file if needed)
- `crates/bridge/src/lib.rs`

## Checklist

### Contract drift and serialization

- [ ] Add a golden test that fails if `bridge-contract.json` no longer matches the Rust bridge traits, method names, argument order, optionality, and raw syscall families.
- [ ] Add deserialization tests for the contract structs in `crates/bridge/src/lib.rs` that prove malformed JSON, missing required fields, and unknown convention values fail cleanly.
- [ ] Add focused shape/invariant tests for bridge-facing value types such as permission decisions, lifecycle payloads, structured events, and execution records instead of implying full serde round-trips for types that are not serialized in this crate today.
- [ ] Add compatibility tests that contract field names and enum/tag values consumed by guest bridge bootstrap code stay stable across schema changes.

### Surface completeness

- [ ] Add a coverage test that asserts every raw syscall family described in the contract has a corresponding typed Rust entrypoint and guest bridge export.
- [ ] Add a test that bridge globals and inventory metadata stay in lockstep with the contract manifest and fail loudly on rename or removal.
- [ ] Add a test that permission-decision payloads cover allow, deny, prompt, and structured-reason variants.
- [ ] Add focused value-shape tests for lifecycle, clock, random, and structured-event records so bridge consumers fail loudly if required fields or variant coverage drift.

### Negative and compatibility cases

- [ ] Add malformed contract-manifest fixture tests for each major bridge group so deserialization failures are deterministic and non-panicking.
- [ ] Add version-skew tests where newer manifests include fields older consumers do not know about, including unknown enum values and new optional fields.
- [ ] Add tests proving large contract versions, empty method-name arrays, and null-bearing optional manifest fields are handled or rejected explicitly.
- [ ] Add regression tests for contract ordering so generated bridge-global/bootstrap artifacts remain deterministic across rebuilds.
