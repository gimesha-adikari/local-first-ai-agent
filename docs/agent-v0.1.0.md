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

## Part 04 — calculator and device tools

The new agent-tools package lives in crates/tools. Its public dispatch
takes ownership of a non-cloneable AuthorizedCall, checks dispatch liveness,
and uses the registered timeout plus cooperative task checks before, during
and after native work. The supported registry exposes only the calculator
and, on Linux, device.info. A contract without an adapter cannot execute.
The host must persist the authorization audit before dispatch and run native
work off the UI thread. Native syscalls are not forcibly interrupted.

The calculator accepts finite f64 arithmetic: decimal/scientific literals,
parentheses, unary signs, +, -, *, / and % (remainder). It uses a bounded
recursive-descent parser with 256 input bytes, depth 24 and 128 parse steps,
rejects division by zero and non-finite intermediates, and does not execute
code or support variables, functions or exponentiation operators. Floating
point is approximate, not exact financial or arbitrary-precision arithmetic.

Linux device.info uses fixed procfs entries and statvfs("/") without shell
commands. It reports Linux and architecture, a descriptive CPU name when
available, process-available logical parallelism, kernel memory totals and
available memory when present, and root filesystem capacity/available space
when obtainable. It does not enumerate private files, hostnames, serial
numbers or machine IDs. GPU is explicitly unavailable (null) in this adapter;
a reliable GPU naming source and detailed distro detection are deferred.
Values describe the current execution environment, not necessarily the
physical host or all mounted volumes.

The workspace and Cargo.lock were updated at source level for agent-tools
and its Linux-only direct use of the already locked libc 0.2.191. No new
third-party package/checksum was introduced. The package inherits the
baseline workspace version until the milestone version update. Cargo
resolution, formatting and native build remain unverified.

Nine ordinary test cases and one opt-in Linux hardware snapshot case are
authored, not executed. The agent runtime and desktop are not yet wired to
the adapters.

## Part 05 — Linux scoped filesystem tools

NativeTools now owns a session FilesystemScopes registry. Trusted host code
must use grant_selected only after a native picker and bind the resulting
opaque policy ID to a pinned O_PATH descriptor. Revoke invalidates the
policy lease and removes the stored handle; prune releases expired handles.
The legacy stateless dispatch wrapper cannot access scoped resources.
Linux now exposes four supported tools. Other platforms still expose only
the calculator. The desktop picker and runtime wiring remain future work.

All descendant paths use openat2 with RESOLVE_BENEATH, RESOLVE_NO_SYMLINKS,
RESOLVE_NO_MAGICLINKS and RESOLVE_NO_XDEV; there is no weaker fallback.
Selection requires an absolute non-root path with no symlink components.
Only selected conventional filesystem types (ext, XFS, Btrfs, tmpfs,
overlay, exFAT, FAT) are accepted. Network/FUSE/pseudo-filesystems are
not supported. Files must be regular and singly linked. A candidate is
pinned before opening for read, then reopened through a checked procfs
descriptor directory and verified by device/inode/type/link count. No
device, socket or FIFO is opened for data access. The process and its
mount namespace/procfs are assumed trusted; this is not an OS sandbox.

Search enumerates directory descriptors and re-resolves each candidate
from the original root handle. It returns relative UTF-8 regular-file
paths, with ASCII case-insensitive name/extension filters, at most 100
results, depth 8, 10,000 visited entries and a bounded serialized result.
Skipped inaccessible/unsafe entries, depth or size limits may mark results
truncated. Search is not a snapshot; concurrent renames may omit results.
The selected root/file remains the authorized object if renamed; a
replacement at its old path is not authorized by a file grant.

Read validates all source bytes (max 64 KiB), rejects invalid UTF-8 and
control bytes other than tab/newline/carriage return, and returns a preview
of at most 4096 bytes, shortened for JSON escaping with explicit truncation.
Reads use the pinned regular descriptor; metadata changes during read are
rejected on a best-effort basis. Concurrent writes, hard-link creation and
renames cannot be treated as a content snapshot or absolute provenance
guarantee. Cooperative checks bound work but cannot forcibly interrupt
blocking syscalls. User-selected directories may contain sensitive files
and their contents remain untrusted data; no instructions in them confer
authority. Host must persist audit records, retain trusted scope ownership,
and not log private paths or content.

Linux openat2 availability, bind mounts, adversarial race tests and actual
desktop behavior remain deferred to verification. Six integration test
cases were authored, not executed; no additional dependency was added.
Reference: https://man7.org/linux/man-pages/man2/openat2.2.html

## Implementation sequence

| Part | Scope | Status |
| --- | --- | --- |
| 00 | Pinned baseline review | Source reviewed |
| 01 | Domain, lifecycle, errors, budgets | Implemented; unverified |
| 02 | Tool registry and validation | Implemented; unverified |
| 03 | Permission engine | Implemented; unverified |
| 04 | Calculator and device tools | Implemented; unverified |
| 05 | Linux filesystem tools | Implemented; unverified |
| 06 | Linux application tools | Pending |
| 07 | Model decision protocol | Pending |
| 08 | Bounded execution runtime | Pending |
| 09 | SQLite task persistence | Pending |
| 10 | Tauri commands and events | Pending |
| 11 | Desktop agent UI | Pending |
| 12 | Static integration review and handoff | Pending |

The calculator, Linux device and scoped filesystem adapters exist as source, but UI integration,
persistent permission audit and model-driven execution remain pending. The
core policy and native dispatcher must be wired into the trusted runtime
before any action is available in the application.
