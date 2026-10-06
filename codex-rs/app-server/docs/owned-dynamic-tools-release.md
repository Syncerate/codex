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

## Qualified build and package

The frozen build source is `216a55698e01760e5c7e27e0349e945bcba96d4e`, tree
`4775a37ca9d02d68b4fc64ef563171775fb030f8`. Later documentation commits are not
build sources. Rust/Cargo 1.95.0, Bazel 9.0 and two-job compilation produced the
complete musl CLI and code-mode host. The voice native recipe received two jobs;
its default eight, override two and invalid zero configurations were checked.
Release debug information was disabled; the canonical symbol extraction and
strip script was applied to fresh executable copies. Native voice uses the
release GNU helper contract and records the same source commit.

The package contains 44 regular files, totaling 447,340,469 bytes. Every regular
artifact, mode, manifest and archive digest is recorded in the retained
`full-package-inventory.json` qualification artifact. A generic copy of all
package entries is committed in [owned-dynamic-tools-release-artifacts.json](owned-dynamic-tools-release-artifacts.json).
Principal digests are:

| Artifact | SHA-256 |
| --- | --- |
| Packaged musl CLI | `a6d926a8025c0ce6e134138be0ae48488be8732be8095e07071582301d23651d` |
| Packaged musl code-mode host | `a4350be302671110827ad5cc6b87580f5d6615ff71a37288a4da709287de661f` |
| Rebuilt voice host | `4bd45ce47fb84e27eb6ba705cb8215309a97470a442f085f6250f9bfd117e819` |
| Full package tar.gz | `da2320c6588887cdbff8b3bb2baad9fbdc1cf50492bb895569f6f27de538da27` |
| Full package tar.zst | `b8a4d8979bacd3b64cb2db594247eb49631855287484b7da573706bcacc1ca9c` |
| Symbol archive | `ed41f8d6eb658a7a1266f15fd93922f45835481bc079525ee135ee8e87ea8618` |
| Full inventory JSON | `6bda2e9f6d1e6b88909a617f0ea5a3c691cd038104d05ec83c79400538eeea0e` |

Ripgrep, bubblewrap and zsh were reused byte-for-byte from the exact release
artifacts and checked with their version commands. Voice was rebuilt rather
than relabeled. Its build-commit handshake, runtime initialization, shutdown,
and wrong-commit rejection passed. The packaged code-mode host completed a
framed evaluation returning 42 and shut down cleanly. These helper probes do
not qualify audio devices, interactive zsh or bubblewrap sandbox execution.
The candidate package is unsigned; it is not an official release signature.

## Native and managed receipts

On the frozen source, native catalog, protocol, ownership and routing tests
passed: 723 tests total (705 library tests, one core catalog regression and 17
app-server integration tests), with one intentional fixture-writer ignore.
Stable SDK and experimental schemas were regenerated; stable exports stayed
identical to the release base. Formatting and Bazel lock checks passed.
Scoped Clippy completed with warnings. Its proposed ownership match rewrite
changed semantics, so generated fixes were preserved separately and reverted
before building the exact tested source. This is not a warning-free lint claim.

The protected Go mock-broker managed fixture passed with race detection against
broker commit `dd6008183cd89e5c9cadb5774f7276b6e1084182`:

```sh
BROKER_TEST_MANAGED_CODEX_CLI="$FORK_PACKAGE/bin/codex" \
BROKER_TEST_MANAGED_DRIVER="$QUALIFICATION_DRIVER" \
GOTOOLCHAIN=local GOROOT="$GO_ROOT" GOMAXPROCS=2 GOCACHE="$TEMP_GO_CACHE" \
go test -p2 -race -count=1 -v ./broker \
  -run '^TestCodexManagedSharedFullPackageAdapter$'
```

The latest testcase passed in 69.07 seconds and checked exactly two mock effects and
one revoked binding. Four managed-boundary and fifteen controller/Unix Python
tests also passed. The external driver used only private disposable homes and
mock admission, with auto-update disabled and no credentials. Actual managed
executable hashes were checked, rather than relying on the shared version string.

The exact stock 0.160.1 proxy observed item events but no owned callback. Forged
result/error replies were denied; owner reconnect retained its capability.
Managed restart changed the verified process generation. Explicit administrator
restore preserved the broker epoch and operation IDs; reconciliation did not
replay the uncertain mutation. Disconnected revoked cleanup reclaimed solely
for clear without reactivating broker authority.

Both patched installation and rollback used the selected package's own
`app-server daemon update --from-cli --yes`. Exact stock CLI digest was
`f34a4d2301892ae96c90097786bfe5dc269f187b6f69faf42a7b357b8c081e35`;
stock archive digest was
`05f9279fcfb76564a1801dd835286a56277c0a4dd97d94f58543e71b2ed5f80c`.
All 44 regular files matched the installed release inventory. Its additional
installed `codex -> bin/codex` alias is absent from the public archive and was
not silently added. The stock package was preserved outside managed selectors.

Stock reopened the same fork-written home, listed/read/resumed both ordinary and
formerly owned threads, preserved existing turns, completed new turns and
passed a second cold resume. Migration sources were unchanged from the release
base and observed ledgers remained unchanged. A representative extra attachment
index was manually added only to the disposable store; stock retained it. This
port does not introduce that index or a newer snapshot migration.

The final receipt records stopped fixture daemon PID/start identities, reaped
stock proxy processes, a closed canonical socket and a stopped mock provider.
Earlier driver-only failures (version output without PID and concurrent proxy
close locking) were corrected in temporary qualification code; the runtime and
broker source were unchanged. Full receipts and logs are retained with the
package inventory. Actual stock proxy protocol behavior is qualified; TUI
rendering, closed-source desktop UI, live state scale and older GNU helper host
compatibility remain untested.

A live swap requires a separate operator window and authorization, verified
backups and exact rollback package, daemon-owner canonical paths, disabled
managed auto-update, and explicit ownership activation/restore procedures.
This preparation performed no live swap or user-home lifecycle operation.

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

The later adapter shutdown fix was independently qualified against the unchanged
package. Its idle Unix socketpair regression verifies that stream shutdown wakes
the reader before cleanup. The earlier successful managed receipt and failed
driver diagnostics are retained; this rerun replaces the adapter fixture pin,
not the frozen build source or any package digest.
