# NyaTerm fork notes

This branch carries [NyaTerm](https://github.com/nyakang/nyaterm)'s hardening of
`vnc-rs` on top of an unmodified upstream base.

- Fork: <https://github.com/nyakang/vnc-rs>
- Upstream: <https://github.com/HsuJv/vnc-rs>
- Base revision: `ab684d009d767c968af2f7559576334038623124` (`vnc-rs` 0.5.3,
  also upstream `main` head at the time of this branch)
- Branch: `nyaterm`

NyaTerm links this crate only from `nyaterm-vnc-helper`, an isolated helper
process that decodes VNC on behalf of the application. That process boundary is
what keeps this parser — and the `flate2`/`image` decoders it pulls in — away
from server-controlled bytes in the main process. Raw must remain the required
fallback, and ZRLE/Tight should only be advertised where the corresponding
decoder tests and interoperability checks pass.

## Patches

1. `feat: add configurable protocol limits and typed protocol errors` — the
   `VncLimits` type and the `VncError` variants the parsers need to reject
   malformed input as ordinary errors.
2. `fix: bound server-controlled sizes and remove unsafe and panicking paths` —
   `#![forbid(unsafe_code)]`; removes the `set_len` uninitialised buffer, the
   `ptr::copy_nonoverlapping` writes in Tight, and the `SecurityType` /
   `AuthResult` transmutes; replaces `unreachable!`, `unimplemented!`, slice
   indexing and `unwrap` on server input with typed errors; bounds every
   server-chosen size and routes offset arithmetic through `checked_mul`.
3. `feat(codec/zrle): emit one image per rectangle instead of one per tile`.
4. `feat(codec/tight): decode JPEG in-crate with bounded allocations` — decodes
   through `image` with `Limits` pinned to the declared rectangle instead of
   handing raw JPEG bytes to the consumer.
5. `feat(client): add an explicit security-type selection policy` —
   `VncSecurityPolicy::{Auto, NoneOnly, VncAuthOnly}`.
6. `test: add handshake, decoder, and limit regression tests`.
7. `deps: narrow the tokio feature set and drop the release profile override`.
8. `style(codec): clear palette and pixel buffers with clear()` — `truncate(0)`
   tripped clippy's `manual_clear`, which postdates the code and fails
   `-D warnings` on the base revision too. Behaviour is identical.
9. `test(codec): satisfy clippy on test targets` — struct update syntax for the
   `VncLimits` test values, and `zrle.rs`'s test module moved to the end of the
   file.

## Not carried here

Nothing functional. The `vendor/vnc-rs` snapshot in the NyaTerm tree differs from
this branch only by its own `VENDOR.md` and a retained `Cargo.lock` (this
repository gitignores the lock file).

## Validation

`.github/workflows/nyaterm.yml` runs this on every push to the branch, because
upstream's `build.yml` only triggers on `main`. These tests used to run inside
NyaTerm's own `cargo test --workspace` while this crate was a path dependency in
that workspace; NyaTerm now consumes it as a pinned git dependency, so this
branch is the only place they run.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features            # 24 passed
cargo test --release --all-features  # 24 passed, and release matters: an
                                     # unchecked overflow panics in debug but
                                     # wraps in release
cargo test --doc --all-features
cargo build                          # plus windows and wasm32-unknown-unknown
```
