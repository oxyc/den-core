# den-core

Shared, pure Den client policy. Not an Atlas addon, relay, daemon, or sync worker.

`crates/den-sync` owns wire-v2 merges, logical-clock issuance, explicit action capture, event-to-command
translation and supersession, provider preflight decisions, and capped retry scheduling. Every binding
calls the same versioned JSON API. Invalid input is an error, never an empty state or an acknowledgement.

Clients retain encryption, SQLite/browser storage, credentials and account binding, provider HTTP, receipts,
UI, and scheduling. They must recheck account/current intent immediately before HTTP and persist successful
receipts. Provider APIs have no atomic compare-and-set: the final read/write race is not exactly-once delivery.

## Packages and builds

Rust 1.95.0, UniFFI 0.29.5, wasm-bindgen/CLI 0.2.128; dependency resolution is pinned in Cargo.lock.
On macOS with Xcode, install Rust targets `aarch64-apple-darwin`, `aarch64-apple-tvos`,
`aarch64-apple-tvos-sim`, `aarch64-apple-ios`, `aarch64-apple-ios-sim`, and `wasm32-unknown-unknown`.
Install `wasm-bindgen-cli --version 0.2.128 --locked`, then run:

```sh
cargo test --locked -p den-sync
bash scripts/build-bindings.sh
```

The Swift package contains a generated UniFFI wrapper and an XCFramework for ARM64 macOS, Apple TV and
iOS (device and simulators). Intel hosts/simulators are not packaged. The web module loads its separate WASM
asynchronously with `initialize()`, which concurrent callers share and can retry after failure. Call it before
the synchronous `evaluate()` API. Browsers can preload on idle and await readiness before actions or merges.
Production CSP must allow `'wasm-unsafe-eval'`, not general JS eval. Offline use requires the binary to be
available in the browser cache or already initialized; there is no embedded fallback.

With `den`, `den-edge`, and `den-core` sibling workspaces, `bash scripts/build-bindings.sh --vendor`
mechanically copies packages into `den/Vendor/DenCore` and `den-edge/web/src/vendor/den-core`.
**Client builds use those checked-in artifacts, not a sibling checkout, Rust installation, or CDN.**
Each package's `SOURCE.json` pins source/dependency hashes, tool versions, and every artifact hash.
Regenerate both packages together; do not hand-edit generated policy or bindings. Native binary debug
symbols are stripped in packaging; original build outputs remain under `target`.

## Verification

The wire fixtures come from den-spec. `tests/fixtures/policy-v1.json` is the same hand-authored contract
run through Rust, the real native library in DenKit tests, and real browser WASM in web tests.
DenKit also exercises current-state supersession, outage replay, receipt persistence, and conservative
sub-millisecond remote ordering. Web tests exercise offline encrypted journal recovery. Run the browser
CSP/offline smoke with `node test/sync-policy-browser.mjs` from den-edge/web after installing Playwright.

No stored history is migrated, cleared, or compacted by this extraction. Wire-v2 rows and tracker event-v1
IDs/timestamps remain unchanged. Signed historical timestamps are supported. Policy errors pause affected
work; the journal remains the recovery source. No sync worker is implemented or deployed.
