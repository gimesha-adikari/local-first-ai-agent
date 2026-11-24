# Agent MVP v0.1.0 — implementation record

Baseline: 7b417411b1905de66d37f62438564f1d3bc67926 (verified offline chatbot).
Development branch: feature/agent-v0.1.0.
Verification status: implementation-only; local builds and tests deferred.
Existing push-triggered CI remains unchanged.

## Scope

Separate ordinary chat from agent mode. Introduce six registered tools:
math.calculate, device.info, files.search, files.read, apps.list, apps.launch.
All proposed actions pass deterministic validation and authorization.
No shell execution, file mutation, internet tools, or autonomous background jobs.

## Part 01 — task domain

The shared core exposes agent task IDs, ordered state transition records,
terminal states, structured errors, and per-task execution budgets. Tasks are
created by the host, not deserialized from model output. Valid transitions are
checked; terminal tasks cannot resume. A transition is not proof of permission
or successful execution. The future runtime is responsible for those checks.

The default limits are hard MVP ceilings: five tool dispatches, eight model
invocations, two recoverable retries, 2048 generated tokens, and 120 seconds
of total elapsed time. Approval waits share the task deadline and may be
configured shorter. A cancelled or timed-out invocation must still settle
its token reservation. Failed invocations with unknown usage are charged
their full reservation. The runtime must enforce actual context limits and
interruptible operation deadlines. The budget object itself is not a watchdog.

Task IDs use a nonzero u64 internally; the host must allocate persistent IDs
and use a lossless IPC representation (string or a checked safe integer).
Do not treat IDs or transition events as authority tokens.

Four model-free integration test cases are authored, not executed.
No new dependencies or lockfile changes are needed for Part 01.

## Part 02 — tool registry and validation

The registry starts empty and rejects unknown or unregistered identifiers.
It can register the six fixed MVP contracts; registration declares a contract,
not the presence of a native adapter or user permission. Later runtime
integration must register only capabilities it can actually execute.

Each contract has input and output JSON Schema descriptions, risk,
permission requirement, cooperative timeout, output bound and side-effect
description. Typed Serde validation and explicit byte/semantic checks are
authoritative; the schemas are model guidance, not the enforcement engine.
All proposal fields are checked, with raw argument JSON retained to reject
duplicate keys. No text scanning or fenced JSON extraction is performed.
The future inference adapter must call this parser only for a complete
dedicated tool decision, never ordinary chat, reasoning, or partial output.

A validated call is immutable through its public interface and is not an
approval. Scope and application identifiers are opaque ASCII identifiers
of up to 64 bytes; trusted native adapters allocate and resolve them.
File paths are lexical relative paths only; the native layer still needs
descriptor-relative containment and file-type checks. File search defaults
to 100 results and depth 8 (depth zero means immediate entries).
File reading accepts a file scope or a directory scope plus relative path,
with a 64 KiB source-file limit and up to 4096 bytes of UTF-8 preview.
The entire serialized observation must fit 8192 bytes, including escaping;
adapters must shorten previews/lists with explicit truncation or fail closed.
The tool result type must match the validated call. Launch success means
dispatch accepted, not confirmation of an opened window or completed task.

The only dependency change is a direct reference to the already locked
serde_json 1.0.154 with its raw_value feature. The existing package checksum
and transitive dependencies are unchanged; the agent-core dependency list
in Cargo.lock was updated at source level, not resolved by Cargo.
Five regression tests are authored, not executed.

## Part 03 — permission engine

The core now has an in-memory permission engine with default-deny registry
checks, session resource grants, resource-kind matching, a single active task,
and a single pending exact operation. A trusted host must supply a fresh
unpredictable session namespace and monotonically allocated task IDs. The
engine generates non-reused scope and operation identifiers. Scope grants are
read-only, limited to 128 per session and at most eight hours; they are not
persisted or restored. Revoked IDs are retained until session end.

Automatic and scoped-read operations receive one-use authorization tickets.
Every application launch requires a separate explicit human confirmation.
The engine stores the exact validated call and matches it again when the
ticket is consumed; replacement arguments, replay, mismatched task IDs,
expired grants and unconfirmed operations do not authorize dispatch.
Approval is not a reusable application permission. Missing scope is denied;
the host may separately offer an explicit native picker and retry with a
new validated proposal after selection. No model input may grant a scope,
respond to confirmation, or directly execute a tool.

A consumed AuthorizedCall is non-cloneable and has cooperative liveness
checks for task cancellation, task deadline, scope expiration and revocation.
The trusted executor must take ownership, dispatch at most once, check_dispatch immediately before native access
(including the original approval deadline), and check_live during bounded work, and report uncertain side
effects accurately. Revocation cannot undo a dispatched action. The engine
does not open files or implement native containment. The host must bind IDs
to immutable native handles; application IDs must resolve to a stable trusted
discovery target shown in the approval UI and revalidated before launch.

Metadata-only audit records are held in a bounded 1024-record buffer.
New authority fails closed when full; revocation and task closure are still
effective, and any omitted events are counted. The host must serialize audit
draining and persist the relevant records before dispatch, fail closed on
persistence failure, and retain the omission count. No document contents,
absolute paths or arguments are included. Audit persistence and host IPC
wiring remain future work. No new dependencies were added.

Five integration and two unit test cases are authored, not executed.
The tests cover replay, argument substitution, denial, resource kind,
revocation, cancellation, expiration, audit capacity and engine teardown.

## Implementation sequence

| Part | Scope | Status |
| --- | --- | --- |
| 00 | Pinned baseline review | Source reviewed |
| 01 | Domain, lifecycle, errors, budgets | Implemented; unverified |
| 02 | Tool registry and validation | Implemented; unverified |
| 03 | Permission engine | Implemented; unverified |
| 04 | Calculator and device tools | Pending |
| 05 | Linux filesystem tools | Pending |
| 06 | Linux application tools | Pending |
| 07 | Model decision protocol | Pending |
| 08 | Bounded execution runtime | Pending |
| 09 | SQLite task persistence | Pending |
| 10 | Tauri commands and events | Pending |
| 11 | Desktop agent UI | Pending |
| 12 | Static integration review and handoff | Pending |

No native tool execution, UI integration, persistent permission audit, or
model-driven execution exists as a result of Parts 01–03. The core policy
must be wired into the trusted runtime before any action is available.
