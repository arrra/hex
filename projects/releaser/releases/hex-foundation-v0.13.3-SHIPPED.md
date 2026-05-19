# hex-foundation v0.13.3 — SHIPPED

**Status:** SHIPPED as v0.13.3 (718ba66)
**Shipped:** 2026-05-06
**GitHub Release:** https://github.com/mrap/hex/releases/tag/v0.13.3
**SA-030 sign-off:** PASS (Sentinel, 2026-05-06)

## What shipped

The v0.13.2 content (SA-030 PASS) shipped as v0.13.3 because 2 additional
commits landed after the v0.13.2 tag (Cargo.lock + staged notes) requiring
a version bump, and during release.sh the regression suite revealed that
two health scripts were missing from the sync:

### Fixed (v0.13.2 content, SA-030 covered)
- session-start.sh: channel checkpoint resume
- hex-integration-check.sh: export _error_raw bug + emit throttle
- memory_index.py: cascade-delete vec_chunks orphans
- memory_search.py: _rrf_merge FTS-only documented
- check-career-pipeline.sh: policy validation fix + sanitize clean
- hex-doctor: two new health module stubs (Vector Search, hex-events Policy Load)

### Added (discovered during release pipeline)
- check-hex-events-policy-load.sh: surfaces POLICY LOAD/VALIDATION ERROR entries
- check-vector-search.sh: verifies sqlite-vec loadable + memory.db has vectors
- test-doctor-events-coverage.sh: updated to test new external-script architecture

### Deferred to v0.14.0
- messaging.rs MessagingHandler.receive() + wake.rs crash-recovery inbox
  (type mismatch: messaging::Message vs types::Message)

## Gates
- [x] Sentinel SA-030 sign-off
- [x] release.sh — all gates PASS, pushed 718ba66
- [x] GitHub Release — https://github.com/mrap/hex/releases/tag/v0.13.3
- [ ] Brand Lead notification
