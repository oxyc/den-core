#!/bin/bash
# Build generated transport only; all policy stays in crates/den-sync. No installation/deployment.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
if [[ -x "$root/.tools/cargo/bin/rustup" ]]; then
    export RUSTUP_HOME="$root/.tools/rustup"
    export PATH="$root/.tools/cargo/bin:$PATH"
    export RUSTUP_TOOLCHAIN=1.95.0
fi
# No arguments does everything. CI runs the parts on separate runners:
#   --only checks|native|wasm   run just that part (checks = fmt + the whole workspace's tests;
#                               native = host dylib, uniffi-bindgen, Apple targets, xcframework; wasm = web modules)
#   --skip-tests                leave the tests out of the checks part (fmt is still checked)
only=""
skip_tests=0
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    case "${args[i]}" in
        --only) only="${args[i + 1]:-}"; i=$((i + 1)) ;;
        --only=*) only="${args[i]#--only=}" ;;
        --skip-tests) skip_tests=1 ;;
        --format | --vendor) ;;
        *) echo "unknown argument: ${args[i]}" >&2; exit 2 ;;
    esac
done
case "$only" in
    "" | checks | native | wasm) ;;
    *) echo "--only takes checks, native or wasm, not '$only'" >&2; exit 2 ;;
esac
want() { [[ -z "$only" || "$only" == "$1" ]]; }

if want checks; then
    if [[ " $* " == *" --format "* ]]; then cargo fmt --all; fi
    cargo fmt --all -- --check
    # The whole workspace, not just den-sync: `default-members` is den-sync alone, so a bare `cargo test`
    # silently skips den-store.
    if [[ $skip_tests == 0 ]]; then cargo test --locked --workspace; fi
fi

if want native; then
    # `build-std` below must rebuild the standard-library objects with the same deployment target as Den's
    # objects. The toolchain is pinned above/in CI, and rustup makes this component install idempotent.
    rustup component add rust-src --toolchain 1.95.0
    # Rust 1.95 can race loading freshly linked proc-macro dylibs (E0463) in optimized native builds. Keep this
    # packaging path serial until the pinned toolchain no longer needs the workaround.
    cargo build --locked --release -j1 -p den-core-native --lib
    mkdir -p Swift/Sources/DenCore Swift/Headers Swift/Artifacts .build
    cargo run --locked -j1 -p den-core-native --features bindgen --bin uniffi-bindgen -- \
        generate --library target/release/libden_core_native.dylib --language swift \
        --config bindings/native/uniffi.toml --out-dir .build/generated-swift --no-format
    cp .build/generated-swift/DenCore.swift Swift/Sources/DenCore/DenCore.swift
    cp .build/generated-swift/DenCoreFFI.h Swift/Headers/DenCoreFFI.h
    cp .build/generated-swift/DenCoreFFI.modulemap Swift/Headers/module.modulemap

    scratch="$(mktemp -d "$root/.build/xcframework.XXXXXX")"
    # Namespace headers: Xcode flattens static-library header roots and otherwise collides with LibDovi.
    mkdir -p "$scratch/headers/DenCoreFFI"
    cp Swift/Headers/DenCoreFFI.h Swift/Headers/module.modulemap "$scratch/headers/DenCoreFFI/"
    targets=(aarch64-apple-darwin aarch64-apple-tvos aarch64-apple-tvos-sim aarch64-apple-ios aarch64-apple-ios-sim)
    # Cargo 1.95 accepts only one --target per build. Build sequentially so each archive is produced reliably;
    # Isolate cross-build host-side proc macros from the preceding host/bindgen build while reusing them across
    # Apple targets and CI runs. Keeping this under target/ lets rust-cache preserve the five expensive build-std
    # trees; the old random scratch path forced every run to rebuild them from nothing.
    cargo_target="$root/target/native-cross"
    for target in "${targets[@]}"; do
        # The wrapper sets the deployment environment only for rustc invocations carrying this target. Host-side
        # proc macros therefore remain loadable on the packaging Mac. Rebuild the pinned standard library too:
        # otherwise its precompiled objects retain Rust's lower platform defaults inside our static archive even
        # though every workspace/dependency object has the requested deployment target.
        RUSTC_BOOTSTRAP=1 RUSTC_WRAPPER="$root/scripts/rustc-apple-deployment.sh" \
            CARGO_TARGET_DIR="$cargo_target" \
            cargo -Z build-std=std,panic_unwind build --locked --release -j1 \
                -p den-core-native --lib --target "$target"
        mkdir -p "$scratch/$target"
        cp "$cargo_target/$target/release/libden_core_native.a" "$scratch/$target/"
        xcrun strip -S "$scratch/$target/libden_core_native.a"
    done
    assert_build_version() {
        local archive="$1" platform="$2" minos="$3" members builds bad
        members="$(xcrun ar -t "$archive" | wc -l | tr -d ' ')"
        builds="$(xcrun otool -l "$archive" | awk '$1 == "cmd" && $2 == "LC_BUILD_VERSION" { n++ } END { print n + 0 }')"
        # `ar -t` includes the archive symbol table; every actual Rust object must carry LC_BUILD_VERSION.
        [[ "$builds" -eq $((members - 1)) ]] || {
            echo "$archive: expected $((members - 1)) LC_BUILD_VERSION commands, found $builds" >&2
            exit 1
        }
        bad="$(xcrun otool -l "$archive" | awk -v platform="$platform" -v minos="$minos" '
            $1 == "cmd" && $2 == "LC_BUILD_VERSION" { inside = 1; seen_platform = 0; next }
            inside && $1 == "platform" { seen_platform = ($2 == platform); next }
            inside && $1 == "minos" {
                if (!seen_platform || $2 != minos) bad++
                inside = 0
            }
            END { print bad + 0 }
        ')"
        [[ "$bad" -eq 0 ]] || {
            echo "$archive: $bad objects do not declare platform $platform minimum $minos" >&2
            exit 1
        }
    }
    assert_build_version "$scratch/aarch64-apple-darwin/libden_core_native.a" 1 15.0
    assert_build_version "$scratch/aarch64-apple-tvos/libden_core_native.a" 3 18.0
    assert_build_version "$scratch/aarch64-apple-tvos-sim/libden_core_native.a" 8 18.0
    assert_build_version "$scratch/aarch64-apple-ios/libden_core_native.a" 2 18.0
    assert_build_version "$scratch/aarch64-apple-ios-sim/libden_core_native.a" 7 18.0
    xcodebuild -create-xcframework \
        -library "$scratch/aarch64-apple-darwin/libden_core_native.a" -headers "$scratch/headers" \
        -library "$scratch/aarch64-apple-tvos/libden_core_native.a" -headers "$scratch/headers" \
        -library "$scratch/aarch64-apple-tvos-sim/libden_core_native.a" -headers "$scratch/headers" \
        -library "$scratch/aarch64-apple-ios/libden_core_native.a" -headers "$scratch/headers" \
        -library "$scratch/aarch64-apple-ios-sim/libden_core_native.a" -headers "$scratch/headers" \
        -output "$scratch/DenCoreFFI.xcframework"
    if [[ -d Swift/Artifacts/DenCoreFFI.xcframework ]]; then
        mv Swift/Artifacts/DenCoreFFI.xcframework "$scratch/previous.xcframework"
    fi
    mv "$scratch/DenCoreFFI.xcframework" Swift/Artifacts/DenCoreFFI.xcframework
fi

if want wasm; then
    # Three web modules: ordinary policy, assistant crypto and recovery's memory-hard KDF. Optional policy loads only
    # where it is used. The name section only names functions for stack traces; errors come back as JSON either way.
    # Keep Cargo's package feature unification from adding the recovery feature back to the ordinary module.
    cargo build --locked --release -p den-core-web -p den-core-web-assistant --target wasm32-unknown-unknown
    cargo build --locked --release -p den-core-web-recovery --target wasm32-unknown-unknown
    bindgen="$root/.tools/bin/wasm-bindgen"
    if [[ ! -x "$bindgen" ]]; then bindgen=wasm-bindgen; fi
    "$bindgen" --target web --remove-name-section --out-dir web/generated --out-name den_core \
        target/wasm32-unknown-unknown/release/den_core_web.wasm
    "$bindgen" --target web --remove-name-section --out-dir web/generated --out-name den_core_assistant \
        target/wasm32-unknown-unknown/release/den_core_web_assistant.wasm
    "$bindgen" --target web --remove-name-section --out-dir web/generated --out-name den_core_recovery \
        target/wasm32-unknown-unknown/release/den_core_web_recovery.wasm
    # Binaryen's size passes over what LLVM made: every browser Worker compiles its own copy of a module, and in WebKit
    # the machine code is most of what a copy costs. (opt-level "z" saves ~0.5 MB a copy more but runs 12–27% slower.)
    # The version is pinned (CI installs it, checksummed), since another makes other bytes. The features are the
    # ones rustc 1.95 emits for wasm32-unknown-unknown; none is added.
    wasm_opt="$root/.tools/bin/wasm-opt"
    if [[ ! -x "$wasm_opt" ]]; then wasm_opt=wasm-opt; fi
    if [[ "$("$wasm_opt" --version)" != "wasm-opt version 132"* ]]; then
        echo "wasm-opt version 132 (binaryen) is required; found: $("$wasm_opt" --version)" >&2
        exit 1
    fi
    for module in den_core den_core_assistant den_core_recovery; do
        "$wasm_opt" -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
            --enable-mutable-globals --enable-reference-types --enable-multivalue \
            -o "web/generated/${module}_bg.wasm" "web/generated/${module}_bg.wasm"
    done
fi
if [[ " $* " == *" --vendor "* ]]; then
    node scripts/vendor.mjs
    node scripts/test-vendor.mjs
fi
