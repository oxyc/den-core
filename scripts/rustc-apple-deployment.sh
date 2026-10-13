#!/bin/bash
# Cargo RUSTC_WRAPPER: scope Apple deployment targets to target objects, never host-side proc-macro dylibs.
set -euo pipefail
rustc="$1"
shift
target=""
out_dir=""
previous=""
for argument in "$@"; do
    if [[ "$previous" == "--target" ]]; then target="$argument"; fi
    if [[ "$previous" == "--out-dir" ]]; then out_dir="$argument"; fi
    case "$argument" in
        --target=*) target="${argument#--target=}" ;;
        --out-dir=*) out_dir="${argument#--out-dir=}" ;;
    esac
    previous="$argument"
done

# When an Apple target matches the host triple, Cargo omits `--target` from
# dependency rustc invocations. Target dependencies still write below the
# target-triple directory; host proc macros write below `release/deps`. Use
# that isolation to set the deployment target without making host dylibs
# unloadable on an older packaging machine.
if [[ -z "$target" ]]; then
    case "$out_dir" in
        */aarch64-apple-darwin/*) target="aarch64-apple-darwin" ;;
        */aarch64-apple-tvos/*) target="aarch64-apple-tvos" ;;
        */aarch64-apple-tvos-sim/*) target="aarch64-apple-tvos-sim" ;;
        */aarch64-apple-ios/*) target="aarch64-apple-ios" ;;
        */aarch64-apple-ios-sim/*) target="aarch64-apple-ios-sim" ;;
    esac
fi
case "$target" in
    aarch64-apple-darwin) export MACOSX_DEPLOYMENT_TARGET=15.0 ;;
    aarch64-apple-tvos | aarch64-apple-tvos-sim) export TVOS_DEPLOYMENT_TARGET=18.0 ;;
    aarch64-apple-ios | aarch64-apple-ios-sim) export IPHONEOS_DEPLOYMENT_TARGET=18.0 ;;
esac
exec "$rustc" "$@"
