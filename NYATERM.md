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

## Not carried here

Nothing functional. The `vendor/vnc-rs` snapshot in the NyaTerm tree differs from
this branch only by its own `VENDOR.md` and a retained `Cargo.lock` (this
repository gitignores the lock file).

## Validation

```sh
cargo fmt --check
cargo check
cargo test
cargo test --release
```
