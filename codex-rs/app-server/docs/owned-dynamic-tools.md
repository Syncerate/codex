# Experimental connection-owned dynamic tools

The legacy runtime setter is retained. A host integration may opt into
`thread/dynamicTools/owned/set` after initializing with `experimentalApi: true`.
The registration is made by the trusted harness connection; tool arguments,
model output and another subscriber's claimed identity do not select an owner.
This is protocol evidence for a shared app-server, not a claim that any
closed-source desktop client UI was exercised.

## Wire contract

Request parameters are `{threadId, dynamicTools, reconnectTokens?}`. The response
is `{reconnectTokens: {toolName: secretToken}}`, delivered only to the requesting
connection. `dynamicTools` replaces this connection's own set, preserving legacy
tools, other connections' tools, and disconnected reservations. Names are the
**top-level DynamicToolSpec names**: a namespace spec is owned as one group, with
its capability keyed by namespace name and its functions routed to that owner.

Ownership is assigned only when adding a new name. A legacy tool cannot be
claimed by this method. A live connection may update or clear its own tools;
`[]` clears only its live set and retires those capabilities. It cannot replace
another live owner's name, even with that owner's capability. A reconnecting
connection must provide the correct capability for each disconnected name it
requests. Unexpected token keys, wrong tokens and retired tokens are rejected.
Capabilities cannot promote legacy tools or resurrect registrations cleared in
the current server process. An unknown token supplied for a name absent from
both ownership and the legacy catalog is accepted as a fresh-addition hint:
the caller gets a newly minted capability, never authority derived from the hint.
This allows a durable harness journal to survive server process restart without
retrying arbitrary token errors or dropping its saved state.
The response returns the capabilities for the resulting requested set, allowing
a trusted harness to persist them privately before using its tools.

An empty set with tokens for disconnected names is rejected: token keys must
identify requested specs, not deletion targets. To remove a disconnected
reservation after a client restart, resume the persisted thread, reclaim the
spec with its exact saved token, then send an empty set without tokens. These
are two idle catalog transactions. Keep a broker binding revoked and callback
readiness disabled throughout; transport ownership recovery must not activate
broker authority. Retain recovery state if either transaction fails. Tokenless
`[]` before reclaim clears only the new caller's live set and preserves the
reservation; its empty response is not proof of global removal.

Every new registration receives a random per-name capability. Tokens are kept
only in active server ownership state and the sensitive RPC params/response, with
redacted Debug formatting; analytics does not capture this RPC. Tokens never
enter core tool specs, thread items, conversation history or rollouts. Do not log
raw RPC frames or place capabilities in tool schemas, arguments or user-facing
messages. A host must protect its saved capabilities as credentials.
Process-local hashes of every issued or consumed capability remain after clear;
both the replacement token and an accepted old-process hint are rejected for
absent names until process exit. These hashes commit only after the idle catalog
mutation succeeds, so failed or busy mutations cannot poison recovery retries.

## Routing, disconnect and lifecycle

All subscribers continue receiving ordinary item notifications. An owned
`item/tool/call` request is sent only to its owner when that owner is subscribed
in the event's connection set. A nonowner's JSON-RPC result **and** error cannot
consume its callback. Owned requests are excluded from pending-request replay,
even when the subscriber is the original owner. Legacy dynamic tool request
fanout, response handling and replay retain their previous behavior.

Registration does not subscribe its connection. The harness must subscribe with
`thread/start` or `thread/resume` before using its tools. A live owner missing
from the thread event's subscriber set fails closed, even if other subscribers
remain; reconnect recovery never replays an earlier callback.

A disconnected or unsubscribed owner does not trigger broadcast/fallthrough.
Disconnect fails pending owned callbacks. The core receives its normal failed
dynamic-tool response; that failure does not establish that an external broker
mutation was absent. An uncertain operation must be reconciled using the same
operation ID and live origin; the ownership engine never retries/replays it.

Disconnect preserves names and capabilities. Thread unload also disconnects its
owners while retaining reservations in the app-server process. Resume restores
stored startup tools, without silently granting the caller ownership; a harness
must explicitly reclaim its reserved runtime tools with capabilities. Resume of
an already loaded thread preserves its catalog/ownership. Fork creates a new
thread and does not inherit runtime ownership or capabilities. A server process
restart discards this runtime-only registry, so recovery across server restart
requires fresh explicit addition to the newly loaded catalog. A saved old-process
token may accompany that addition only when the name is absent; the response
replaces it with a new capability. Unknown hints cannot claim legacy names,
live registrations or disconnected reservations, and do not authenticate a
restart or prove previous ownership. Existing registrations still require their
current exact capability for reconnect. Current-process issued capabilities
cannot be moved to a new name or fork.

The legacy `thread/dynamicTools/set` rejects a catalog containing owned or
reserved names, rather than overwriting/clearing them. Other ownership setters
are scoped to their caller. Unload/reclaim and idle setters do not alter stored
startup metadata. Core `Op::SetDynamicTools` remains the ordering fence and
rejects changes during an active turn. The server holds its ownership lock across
catalog planning, the ordered core reply, and ownership commit; routing and
connection cleanup use the same lock so no request observes half a catalog.

A lost initial registration reply may leave a reserved name whose newly minted
capability is unknown to the caller. Do not claim the name from another
connection or fabricate a token. Retain the uncertain state for operator recovery;
only the live owner can clear it before disconnect, or an operator can restart the
server process with the documented runtime-state consequences. Unloading a
thread alone preserves the reservation and cannot recover a lost capability.

## Rebase surface

The patch is based on `59976f8baf9b3984bb52456c4c59d088ae248ff5`. Keep the existing
core `Session::set_dynamic_tools_if_idle` and `Op::SetDynamicTools` handler
unchanged when rebasing. Review these integration seams:

- `app-server-protocol/src/protocol/common.rs`: experimental request/response
  variant, thread serialization scope, and generated protocol fixtures.
- `app-server-protocol/src/protocol/v2/thread.rs`: owned setter wire types and
  secret-redacted Debug implementations.
- `app-server/src/dynamic_tool_ownership.rs`: scoped planning/commit, legacy
  catalog guard, random capabilities, disconnected/unloaded reservations.
- `app-server/src/request_processors/turn_processor.rs`: legacy guard and owned
  setter, holding ownership state across the ordered core setter outcome.
- `core/src/codex_thread.rs`: nonsecret runtime catalog read accessor only.
- `app-server/src/outgoing_message.rs`: owner-targeted request queueing, pending
  callback answer gating under the ownership fence, disconnect cancellation,
  owned-only early unload cancellation, and replay exclusion.
- `app-server/src/dynamic_tools.rs`: sanitized callback error logging.
- `app-server/src/message_processor.rs`: owned RPC dispatch and early disconnect
  cleanup before connection RPC drain.
- `app-server/src/request_processors/thread_lifecycle.rs`: automatic unload
  reservation transition and catalog admission fence.
- `app-server/src/request_processors/thread_processor.rs`: explicit unload/teardown
  reservation transition and pending callback cancellation.
- `app-server/src/lib.rs`, `app-server/Cargo.toml`: module/randomness wiring.

Recheck loaded resume, fork and subscription listener lifecycle after an upstream
change. Keep ownership and user-verification auth-revision semantics separate.
If core catalog access or dispatch changes, preserve the single transaction
between idle catalog acceptance and routing-state commit. Do not expose owner
capabilities by attaching fields to DynamicToolSpec or core turn metadata.
Keep `ConnectionRpcGate`'s started-handler drain behavior: connection closure
rejects queued RPCs while an admitted catalog transaction finishes its commit.
Do not abort that transaction between core acceptance and ownership commit.

## Build and evidence

Validated Rust source: `0ede1549cc81dc9245c72965e58deace9f471997`, based on
`59976f8baf9b3984bb52456c4c59d088ae248ff5`. This includes process-restart hint
recovery and process-local capability tombstones, with the original wire shape
unchanged. The native executable built by the integration-test command has
SHA-256 `8757fc60d29f3a37208a540ba4c544f908253620de0f90d66ca5ed8847a689a0`.

Validation used Rust/Cargo 1.95.0 on `x86_64-unknown-linux-gnu`, an unoptimized
build without debug information, two compilation jobs and two test threads.
The toolchain and build artifacts were temporary; no system toolchain was
installed. Reproduce from the repository root using the pinned toolchain:

```sh
export CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export RUST_MIN_STACK=16777216
cargo build --manifest-path codex-rs/Cargo.toml -p codex-rmcp-client --bin test_stdio_server
cargo test --manifest-path codex-rs/Cargo.toml -p codex-app-server-protocol --lib -- --test-threads=2
cargo test --manifest-path codex-rs/Cargo.toml -p codex-app-server --lib -- --test-threads=2
cargo test --manifest-path codex-rs/Cargo.toml -p codex-app-server --test all dynamic_tools -- --test-threads=2
cargo test --manifest-path codex-rs/Cargo.toml -p codex-app-server --lib dynamic_tool_ownership -- --test-threads=2
```

The MCP fixture binary is required by the existing disconnect-during-startup
unit test. A default-stack test run overflowed in the existing canonical
dynamic-tool test; the larger test stack above preserves the native check.

| Check | Native result |
| --- | --- |
| Protocol library and stable/experimental schema fixtures | 323 passed, 1 intentional fixture-writer skip |
| App-server library, including callback/auth/lifecycle and restart regressions | 401 passed, no skips |
| Dynamic-tool integration selection | 18 passed: 6 owned, 11 legacy, 1 nonexperimental API case |
| Ownership state selection, including disconnected cleanup and restart retirement | 7 passed, no skips (included in the library total) |
| Changed Rust files, documentation shell block and whitespace | Scoped rustfmt, bash syntax and git diff checks passed |

The ownership acceptance starts an actual app-server binary on a temporary
loopback WebSocket listener with mocked model responses and disposable homes.
Two subscribed clients prove owner-only callbacks and ordinary item events for
a nonexperimental client. Result/error spoofing, active-turn rollback, scoped
catalogs, token recovery/retirement, disconnect without replay, unload/resume,
fork isolation and absence of capabilities from model catalogs/items/history
are checked. The restart regression kills a real process with a callback pending,
resumes the same persisted thread in a second process, replaces its old token,
routes only future calls and rejects both capabilities after clear. Stable
protocol exports remain byte-identical to the base revision.

No live daemon, real thread, host account/service/key or installed software was
modified. Closed-source desktop UI and real broker mutations were not exercised;
the executable is ready for a subsequent isolated harness acceptance.
