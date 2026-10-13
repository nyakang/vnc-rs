# NyaTerm fork notes

This branch carries [NyaTerm](https://github.com/nyakang/nyaterm)'s hardening of
`vnc-rs` on top of an unmodified upstream base.

- Fork: <https://github.com/nyakang/vnc-rs>
- Upstream: <https://github.com/HsuJv/vnc-rs>
- Base revision: `99ed1a28553c7594212adf30b9517d57fe46558d` (upstream `main`, 2026-10-13)
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

## RA2_256 alignment (2026-10-04)

Base before this series: `d855f22`. Port Tauri `64d592cf4` RA2_256 handshake,
AES-256 EAX framing, typed key verification callback, credential limits, and
Auto security selection. Explicit None/VNC-only policies remain fail-closed.
The decoder and crypto implementation remain linked only by the VNC helper.

Validation of this patch together with the following trust hardening patch on
Windows: `cargo test --all-features` (40 unit tests and 2 doctests),
`cargo fmt --all -- --check`, and
`cargo clippy --all-targets --all-features -- -D warnings` passed.
Tests use scripted peers, not a real Raspberry Pi/RA2_256 server.

Follow-up: port Tauri `28c711550`. Notify authenticated keys only after full RFB initialization; preserve the 1024–8192-bit RSA range and both documented and TigerVNC random lengths. Regression tests cover rejection, cancellation, timeout, failed authentication, malformed crypto frames, and key notification ordering. Validated with the same 40 tests, 2 doctests, fmt and strict clippy commands above.

## 2026-10-13 upstream rebase

Upstream `main` is fixed at `99ed1a28553c7594212adf30b9517d57fe46558d`.
Previous published NyaTerm head: `689a65166d6440946529b2696314386a70d71d86`.
The previous branch is retained locally as `backup/nyaterm-20261013`.

The effective old-base-to-fork tree (including merge-only resolutions) was
reconstructed byte-for-byte as separate concern commits before rebasing.
Old upstream merge history was flattened; no functional net change was discarded
except the explicitly documented upstream replacements below. The concern
commits and subsequent compatibility fixes remain separate.

| Concern | Reconstructed commit on old base |
| --- | --- |
| decoder dependencies | `e2e7c855c5483f3455e801753c64e665952e321d` |
| bounded protocol handshake and messages | `f0f9709ecde70ad503e2f3fbf0f7aa34b882b5d7` |
| verified RA2 authentication | `c2569a5eeb04e1bb2724242834c9e0a4c7023828` |
| explicit security selection | `73ee92a406af29127f6861ab37d337f60e9c249c` |
| bounded safe rectangle decoders | `f66e8ae099edb440176a534217978b7d48934195` |
| fork validation | `4b4f56ef3b6f1e4294a06313b3642c7fbd45ee76` |
| fork documentation | `f1ba1398cc39d929c68190bdc52e03ba958e990b` |

Replay commits before this documentation update:

- `548df9b8ba66a6ed5a25ff24c4f233b3abe4a1f2` build(fork): preserve decoder dependencies
- `20603e79bf3323f7f5fc033528ac7d0420436153` feat(fork): preserve bounded protocol handshake and messages
- `90cedd9107924eb7ed874d6d6ee061ae8c002a01` feat(fork): preserve verified RA2 authentication
- `d00065962fade40ff0142be989476a56aee8a3b4` feat(fork): preserve explicit security selection
- `c7f7354d3412c211a222b4792e5954551b6a3995` feat(fork): preserve bounded safe rectangle decoders
- `c46652733a7d202ff8be9e5c87560d063d08d87e` ci(fork): preserve fork validation
- `dac888bebdbb32caf5124029a8a306e5caefb087` docs(fork): preserve fork documentation
- `57f83b0bf8ee7dcf6e6db9ab00ccb58a5b2426d7` fix(client): apply protocol limits to upstream resize decoding
- `b5adcb1d441cbfeb750bfaac011faae9f6fe2443` fix(auth): read failure reasons only for RFB 3.8
- `42b32a5324571287321216b98154fed2f6853e6f` fix(codec): retain upstream pixel and palette validation

Now follows upstream 0.6.0, including server names, resize negotiation,
ordered desktop batches and queue cancellation. The safe fork decoders,
configurable resource limits, security policies, RA2_256 AES-EAX transport and
post-initialization authenticated-key notification are retained.
Overlapping upstream decoder hardening is incorporated: validate pixel masks,
reject malformed Tight palettes and preserve upstream malformed-input tests
alongside the fork's decompression/allocation limits. Upstream fixed limits are
replaced with VncLimits at the decoding boundary without changing fork defaults.
LastRect sentinels bound actual decoded rectangles instead of the declared 65535;
the accepted and over-limit cases are covered. Resize validates immutable limits
without taking the event-drain lock. RFB 3.3/3.7 failure framing does not wait for
the RFB 3.8-only reason string; explicit policies and typed security errors remain.

Windows validation passed in both debug and release: 76 unit tests, 1 upstream
authentication integration test and 2 doc tests. All-target strict clippy passed
with all features and no default features; whole-workspace fmt passed. RA2 tests
cover TigerVNC random lengths, unknown subtypes, bad hashes, malformed encryption,
rejection/cancellation/timeouts, initialization failure and notification ordering.


Validation is on Windows; native macOS/Linux checks are delegated to the
branch workflow. Interactive GUI/file-drop and real-server authentication
checks were not performed by this update. Their outcomes must not be inferred
from scripted peers or compile checks.

### CI follow-up: browser randomness

The first 2026-10-13 CI run failed only the wasm32 build: RA2's OsRng pulled
getrandom 0.2 without a browser entropy provider. Enable its js feature only
for wasm32, using Web Crypto and leaving native RNG selection unchanged.
Windows-hosted wasm32-unknown-unknown all-feature build, native strict clippy
and fmt pass. Debug/release tests passed before this target-only dependency
change. The follow-up branch CI verifies Linux/Windows/WASM and all regressions.
