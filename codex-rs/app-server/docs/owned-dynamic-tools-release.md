# Release-based owned dynamic tools preparation

This preparation targets `rust-v0.160.1`, peeled commit
`d27764b82f7118f674371e6d6e76271d9d606edb`. It ports the catalog setter and strict
ownership contract from `69c10c7eb7c9eac542fb64affff53b21dd16befd`. The canceled
unknown-token hint experiment and its revert history are excluded. Unknown,
wrong, unexpected and retired capabilities remain errors. Server-process
restart recovery requires an explicit administrator-requested fresh addition;
ordinary resume must not automatically retry without a capability.

## Port seams

The selected changes are the catalog setter, ownership protocol/state, callback
routing and native tests, followed by the disconnected reclaim/clear regression.
The release port does not import the surrounding newer upstream snapshot.

- Release `Session` lacked the read-only runtime `dynamic_tools` accessor used to
  seed legacy names. Add that accessor beside the idle catalog setter. Preserve
  the ordered `Op::SetDynamicTools` reply and active-turn/realtime fences.
- Resolve the session test end-of-file conflict by retaining the catalog context
  regression, excluding the unrelated later MCP configuration-refresh test.
- Retain release protocol export bytes until canonical schema regeneration;
  do not copy an experimental export containing unrelated snapshot APIs.
- The routing conflict at the end of `message_processor.rs` must not add the
  absent snapshot lifecycle test module. Retain the ownership secrecy check.
  Preserve `ConnectionRpcGate` handler draining and early owner-disconnect
  cleanup, so admitted catalog transactions finish atomically.
- Keep the final disconnected reservation cleanup test and the strict contract
  documentation. The snapshot's earlier native test receipts in
  `owned-dynamic-tools.md` are historical evidence, not release-port results.
- Preserve workspace version `0.160.1` in the Cargo lockfile. The ownership
  dependency patch's snapshot-local `0.0.0` entries must not downgrade it.
- State SQL migrations remain identical to the exact release base. No migration
  or snapshot-only reverse attachment index is imported by this port.

## Full package contract

The primary Linux musl package has a root `codex-package.json` with layout version
1, exact target `x86_64-unknown-linux-musl`, variant `codex`, entry point
`bin/codex`, resource directory `codex-resources` and PATH directory `codex-path`.
Use `scripts/build_codex_package.py` and `scripts/codex_package/layout.py` rather
than presenting a standalone app-server executable as a full CLI package.

Required release assets include the full CLI, code-mode host, ripgrep,
bubblewrap, zsh and the voice helper plus native runtime closure and receipts.
The Linux voice helper is GNU as prescribed by the upstream musl packaging
workflow; this does not permit substituting a GNU CLI for the musl CLI.
Bubblewrap's verified digest must match `CODEX_BWRAP_SHA256` embedded at CLI
build time. Keep the actual patched source commit in build provenance.

The voice handshake checks the exact build commit, and its canonical native
runtime receipt also records that commit. A stock helper/receipt cannot be
relabeled to the patched commit. Build and seal the voice runtime through the
canonical Bazel/release scripts. `//third_party/voice:native_build_jobs` keeps the
upstream default of eight but permits an explicit two-job preparation build:

```sh
bazel build -c opt --jobs=2 --//third_party/voice:native_build_jobs=2 \
  //codex-rs/voice-host:codex-voice-host //third_party/voice:native_runtime
```

Global Bazel scheduling alone does not limit the native recipe's internal
parallelism. Isolate toolchains and caches, and record all regular package
artifact SHA-256 values, target, version, source commit/tree and toolchain.

## Qualification scope

Qualification uses disposable `CODEX_HOME` and `CODEX_SQLITE_HOME`, disables
managed auto-update before the first start, and selects the canonical Unix
WebSocket control listener. It must not read user credentials or operate the
user's shared daemon. Use only new throwaway threads, a mock model and a mock
broker. A stock CLI/proxy observer should see item events without owned tool
callbacks; nonowner result and error replies must not consume an owned callback.

Use the package's own `codex app-server daemon update --from-cli --yes` for both
patched installation and exact stock 0.160.1 rollback into the same disposable
home. Restart recovery is explicit and preserves the broker's durable epoch and
operation IDs; never replay an uncertain mutation. Stock reopening must actually
read/list/resume the fork-written store and complete a supported new turn.
Inspect schema/index differences and parse errors rather than inferring store
compatibility from updater success. Stop disposable processes and verify no
fixture-owned processes remain before reporting a receipt.

This document records the preparation contract. Release-specific compilation,
package assembly, managed update and rollback results remain pending until
immutable receipts are recorded. It does not authorize a live swap or establish
closed-source desktop UI compatibility.

## Preparation receipt

Native source `216a55698e01760e5c7e27e0349e945bcba96d4e`, tree
`4775a37ca9d02d68b4fc64ef563171775fb030f8`, used Rust/Cargo 1.95.0 with two build
jobs, two test threads and a 16 MiB test stack. Canonical stable and experimental
schema generation, Python SDK regeneration, repository formatting, Bazel lock
refresh and the canonical voice build-graph analysis passed.

| Check | Executed result |
| --- | --- |
| Ordered core catalog library regression | 1 passed; 2665 unrelated tests filtered |
| Full app-server library | 389 passed |
| Full protocol library | 316 passed; 1 intentional fixture-writer skip |
| Native dynamic-tool integration selection | Compilation stopped at the configured 2 GiB disk reserve; exit 130; no integration cases ran |
| Full musl CLI/code-mode/voice package | Not built |
| Managed update, stock TUI/proxy and stock rollback | Not executed |

The release's existing unused `ToolCallSource` import warning is unchanged.
Temporary musl compiler/standard library and checksum-verified V8 and OpenSSL
inputs were prepared, but those preparations are not package-build evidence.
The exact stock 0.160.1 musl rollback package was verified and inspected; its
CLI reported version 0.160.1 and a static PIE executable without an ELF
interpreter. No lifecycle command used a real user's Codex home.

The finite blocker is build-storage headroom. Preserve source worktrees and
receipts; do not infer full qualification or publish a deployable artifact from
the passing library subset. Resume native integration, required lint, full
package assembly and disposable managed qualification after storage is available.
The owning broker's exact Unix-transport fixture pin and managed-socket interface
are also required before final adapter acceptance. No release branch was pushed
while these required checks remained incomplete.
