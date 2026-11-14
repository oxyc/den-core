# Repository agent instructions

## Standing rule: parity, performance and discoveries

- These rules apply to every Den addition, fix, removal and behavior-affecting refactor, not only the audit backlog. Default to behavioral and contract parity across iOS/iPadOS, tvOS, browser, LG/webOS and Samsung/Tizen, plus relevant shared services/consumers in the active `den` and `den-*` repositories. Platform-appropriate UI can differ; authority, data semantics, capabilities, notices, recovery and supported outcomes must agree.
- Inspect the equivalent production paths on every target using current code and relevant PRs/issues. Do not assume a shared helper means each host uses it correctly, or that another platform is the reference implementation. Implement the corresponding change everywhere applicable; a correct counterpart may only need verification. For removals, retire matching affordances/routes/capabilities and handle persisted state, caches and older-client compatibility.
- Honor explicit, documented product/build/capability exclusions, such as reduced Tizen functionality. Cite the exclusion and preserve an accurate unsupported state or usable handoff. Existing absence is not evidence of an intentional exclusion. If required parity remains unfinished, link the work and report the change as incomplete for those targets; a follow-up issue alone does not establish parity.
- Review real entry points, caller/model/service/store behavior, role/profile authority, cancellation and stale results, cache keys/invalidation, durable outcomes, localization/accessibility, and success/empty/partial/stale/error states. A missing notice, warning, retry or usable recovery is a functional defect. Check for unreachable/stubbed paths and invented success or fallback logic.
- Performance is a highest-priority concern alongside correctness and privacy. Raise every credible negative effect and concrete opportunity for a gain, including optimizations present on another platform. Consider latency, frames/scroll/focus, CPU, memory, energy, decoders, network/I/O, caches, duplicate work, hidden/background tasks and service throughput/cost. Do not defer performance as cosmetic cleanup or suppress an opportunity because it is not yet measured.
- Distinguish reproduced/source-supported defects, investigations, measured gains and proposed optimizations. Record the mechanism, affected workloads/targets and comparison needed; never invent performance savings. Use matched before/after measurements where feasible. Resolve a regression introduced by the change or obtain explicit acceptance of its tradeoff before calling it complete. Follow existing device-recording instructions.
- Record every additional defect, parity gap, missing meaningful test, performance regression or optimization opportunity discovered while working. Search existing issues/PRs first. Add same-scope findings to the current issue; otherwise update a matching issue or create a focused issue in the owning repository and link it from the originating issue and audit tracker. Include trigger/workload, expected/actual behavior, source evidence, impact, confidence/counterevidence and useful acceptance. File uncertain items as investigations. Creating/linking these issues is explicitly authorized; do not ask permission again. If filing is blocked, retain a complete draft and report the blocker. Keep private implementation evidence out of public issues.
- Prefer E2E or real production-path integration that verifies visible behavior, failure/retry, cancellation/profile replacement and durable readback. Add focused unit tests for meaningful invariants, not mechanical implementation mirrors. Distinguish source/simulator checks from device, engine and performance qualification.
- Completion must include a result for every app target (changed/checked, explicit exclusion, not applicable with reason, or unverified with linked work), actual tests and their limits, performance findings/opportunities, and all new/reused follow-up issues. Do not claim app-wide correctness from a bounded feature check. Reporting discoveries does not automatically authorize unrelated features, deployment or paid pipeline runs.

The shared completion record and ongoing audit are maintained in [Den tracker #481](https://github.com/oxyc/den/issues/481#standing-implementation-rule). These rules stand on their own when the private tracker is unavailable.

## PR #121 receipts-v2 size acceptance

This is the permanent acceptance record for the narrowly authorized size-budget adjustment on
[PR #121](https://github.com/oxyc/den-core/pull/121). It applies only to the required receipts-v2 paths at source head
`65cbf70acaf5e2fd498468d08cd9c9815f8afe6e`; it is not precedent to pin a budget to a failing artifact or to skip
matched measurement for later changes.

The exact-head serial build used Rust 1.95.0, wasm-bindgen 0.2.129, Binaryen 133, the repository `-Oz` pass, and the
three-pass pinned Terser pipeline. All 381 real-WASM contracts passed before the old assistant size assertion stopped
the gate. The immediate parent `5d3404088ace07b2a835b41c7214cee1efbb2801` differs only by rustfmt from the measured
head. The earlier exact pushed parent `487d5cd0499dc7fd161475a0f33aa0d1df9f91e4` measured assistant at 540,732 bytes
and Tizen raw at 2,063,600 bytes; the measured head is +1,305 and +2,440 bytes respectively. No matched parent startup,
deferred, lifecycle-combined, Tizen-gzip, core, or recovery artifact exists, so no delta is claimed for those graphs.

| Gate | Exact-head bytes | Old budget/margin | Accepted budget/margin |
|---|---:|---:|---:|
| assistant WASM | 542,037 | 520,000 / -22,037 | 550,000 / +7,963 |
| startup WASM | 518,219 | 515,000 / -3,219 | 525,000 / +6,781 |
| startup + deferred complete lifecycle | 1,306,131 | 1,300,000 / -6,131 | 1,315,000 / +8,869 |
| reduced Tizen 3 JavaScript raw | 2,066,040 | 2,050,000 / -16,040 | 2,075,000 / +8,960 |
| reduced Tizen 3 JavaScript gzip -9 | 598,803 | 590,000 / -8,803 | 605,000 / +6,197 |

The passing budgets remain unchanged: compatibility core is 839,960/920,000 (+80,040), recovery is
115,987/140,000 (+24,013), and deferred is 787,912/860,000 (+72,088). The accepted thresholds are rounded above the
measured artifacts to retain a small 6-9 KB review signal rather than encoding the current bytes as the budget. Startup
grew by the required raw receipt classifier; the complete lifecycle is the exact 518,219 startup plus 787,912 deferred
sum. This acceptance is a size tradeoff, not a measured latency, CPU, memory, energy, or service-cost improvement.

Required consolidation preceded the adjustment. The PR replaced generated receipt enums and the generic recursive
production visitor with a compact dispatcher, a fixed top-level schema bitset, borrowed `RawValue` slots, and a finite
14-field `u16` nested-operation duplicate scan. It keeps the three safe Terser compression passes. Serial experiments
then showed: a hand lexical scanner worsened assistant/startup/deferred/lifecycle/Tizen raw by 285/141/403/544/195
bytes; extra Binaryen convergence saved only 191 assistant and 332 startup bytes; deleting the core classifier saved
5,631 startup, 4,099 deferred, 9,730 lifecycle and 11,919/3,303 Tizen raw/gzip bytes but broke required handoff and still
failed Tizen; deleting drain saved 12,926 assistant bytes, and deleting drain plus proof saved 21,837 but still left the
assistant 200 bytes over while removing required behavior. Parsing an already-built proof `Value` added 870 bytes.

Every credible lazy boundary was checked before accepting the budgets. A separate receipts module duplicates the
HPKE/X-Wing download, compile graph, linear memory, and wiping owners for clients using assistant plus receipts. A shared
crypto WASM needs an undefined cross-module allocator/secret ABI or copies immutable secrets through JavaScript and
separate memories. Keeping the assistant module as a crypto service loads the oversized graph first and saves nothing;
host-side routing via `JSON.parse` collapses literal/escaped-equivalent duplicates; dynamic/shared-memory/component
linking has no stable wasm-bindgen, cache/version, or reduced-Tizen contract here. Rust `opt-level=z` is also rejected:
the repository records a 12-27% runtime slowdown. No safe lazy or optimizer alternative therefore satisfies all graphs.

Security and privacy remain acceptance conditions. Core/startup/deferred/reduced-Tizen must reject literal and
escaped-equivalent nested duplicates before generic parsing and return the explicit capable-device handoff without
materializing authority. Escaped authority/random/secret normalization must remain in `Zeroizing<String>`; decoded
fixed secrets and opened plaintext remain in wiping owners. No future size repair may weaken exact integers, bounds,
errors, route/key/profile authority, single-key opening, cancellation, offline retry, old-client behavior, or the Tizen
3 exclusion.

The budget edit itself is not final graph validation. At its exact pushed head, rerun serially: repository all-checks,
all 381 real-WASM contracts and each raw/gzip assertion, startup/deferred lifecycle execution and combined size, native
Apple archives/XCFramework and their limits, deterministic vendor packaging/SOURCE hashes, and the complete Tizen raw,
gzip, and runtime-parity graph. Record the resulting exact artifacts and margins; do not merge on this acceptance record
alone if any semantic, package, deterministic-build, or adjusted size gate fails.
