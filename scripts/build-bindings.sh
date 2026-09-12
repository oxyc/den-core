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

if [[ " $* " == *" --format "* ]]; then cargo fmt --all; fi
cargo fmt --all -- --check
cargo test --locked -p den-sync
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
for target in aarch64-apple-darwin aarch64-apple-tvos aarch64-apple-tvos-sim aarch64-apple-ios aarch64-apple-ios-sim; do
    cargo build --locked --release -p den-core-native --lib --target "$target"
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

cargo build --locked --release -p den-core-web --target wasm32-unknown-unknown
bindgen="$root/.tools/bin/wasm-bindgen"
if [[ ! -x "$bindgen" ]]; then bindgen=wasm-bindgen; fi
"$bindgen" --target web --out-dir web/generated --out-name den_core \
    target/wasm32-unknown-unknown/release/den_core_web.wasm
node scripts/embed-wasm.mjs
if [[ " $* " == *" --vendor "* ]]; then node scripts/vendor.mjs; fi
