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
export TVOS_DEPLOYMENT_TARGET=18.0
export IPHONEOS_DEPLOYMENT_TARGET=18.0
export MACOSX_DEPLOYMENT_TARGET=15.0


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
    cargo build --locked --release -p den-core-native --lib
    mkdir -p Swift/Sources/DenCore Swift/Headers Swift/Artifacts .build
    cargo run --locked -p den-core-native --features bindgen --bin uniffi-bindgen -- \
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
    # One invocation, so cargo compiles the targets concurrently instead of one fat-LTO build after another.
    target_flags=()
    for target in "${targets[@]}"; do target_flags+=(--target "$target"); done
    cargo build --locked --release -p den-core-native --lib "${target_flags[@]}"
    for target in "${targets[@]}"; do
        mkdir -p "$scratch/$target"
        cp "$root/target/$target/release/libden_core_native.a" "$scratch/$target/"
        xcrun strip -S "$scratch/$target/libden_core_native.a"
    done
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
    # Two web modules: every op but the assistant's, and the assistant ops with their crypto, which a browser loads only
    # when it needs them. The name section only names functions for stack traces; errors come back as JSON either way.
    cargo build --locked --release -p den-core-web -p den-core-web-assistant --target wasm32-unknown-unknown
    bindgen="$root/.tools/bin/wasm-bindgen"
    if [[ ! -x "$bindgen" ]]; then bindgen=wasm-bindgen; fi
    "$bindgen" --target web --remove-name-section --out-dir web/generated --out-name den_core \
        target/wasm32-unknown-unknown/release/den_core_web.wasm
    "$bindgen" --target web --remove-name-section --out-dir web/generated --out-name den_core_assistant \
        target/wasm32-unknown-unknown/release/den_core_web_assistant.wasm
fi
if [[ " $* " == *" --vendor "* ]]; then node scripts/vendor.mjs; fi
