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

## Part 06 — restricted Linux desktop application tools

NativeTools now owns an ApplicationCatalog. Linux registers all six MVP
adapters. GIO AppInfo discovery and DesktopAppInfo visibility checks identify
eligible system-installed desktop entries. GIO objects are not stored across
threads; only owned candidate metadata is stored in the session catalog.
The direct gio 0.18.4 dependency is already locked; Cargo.lock's agent-tools
dependency list is updated by source edit, not Cargo resolution.

The MVP deliberately supports a narrow launch subset: visible applications
registered under /usr/share/applications, trusted root-owned canonical
desktop files and executable paths with non-writable ancestors, and a simple
Exec executable optionally followed by one file/URI field code. No operands
are supplied. Reject terminal wrappers, D-Bus activation, custom working
directories, shell/interpreter/helper names, complex arguments, unsafe path
characters, setuid/setgid executables and user-installed desktop entries.
Resolve bare executables using /usr/bin, not the model or ambient PATH.
The launcher is reconstructed with GIO from only the vetted absolute
executable; the original desktop arguments are not executed.

Discovery returns bounded name/opaque ID records. Stable IDs refer to
immutable snapshots, are never rebound, and are limited to 256 per catalog.
The host must use approval_target to display the trusted name, desktop file
and executable, not trust model labels. Revalidate the snapshot before
approval and before dispatch. Metadata checks detect ordinary replacement or
package updates, not malicious administrator changes; OS/package trust and
a trusted mount namespace remain assumptions. A fresh approval is required
for every launch, with the original deadline checked immediately before it.

Launch success means GIO accepted dispatch, not that a window opened.
Cancellation after dispatch must not erase a side-effect result. A launch
error may be uncertain; the future runtime must never automatically retry
apps.launch, including on cancellation, timeout, storage failure or an
unknown result. External apps can use the network and access their normal
user environment. The agent does not send prompts, file operands or model
arguments to them. This is not a sandbox. Many Snap/Flatpak, user-installed,
D-Bus-activated and complex desktop entries are intentionally unavailable.
Discovery obtains GIO's system list before applying the 1024 scan cap;
native calls may block beyond cooperative deadlines.

Seven ordinary test cases and one opt-in real discovery case are authored,
not executed. No test automatically launches a real application. Actual
harmless launch and desktop approval checks are deferred to Codex.
API reference: https://gtk-rs.org/gtk-rs-core/stable/0.18/docs/gio/struct.DesktopAppInfo.html

## Part 07 — local model decision protocol

Agent decisions have a separate typed request/response and worker submission
path sharing the existing single-operation inference reservation. Ordinary
chat generation is unchanged. The llama adapter accepts Qwen-style
non-thinking templates using conservative compatibility checks, not a
guarantee of actual model performance. Unsupported providers fail closed.

For this MVP, use a prompted strict JSON protocol rather than native Qwen
function-call messages or grammar-constrained sampling. The embedded chat
template still renders system/user messages with thinking disabled and
greedy sampling. The prompt includes registered tool descriptions/input
schemas, active scope IDs and all prior current-task observations. Output is
buffered privately, bounded at 16 KiB, and accepted only after normal end
of generation. The parser distinguishes final answers from tool proposals,
rejects extra/duplicate fields and delegates argument validation to the
registry. Never extract JSON from prose, reasoning, Markdown or partial
output. Parsing does not authorize or execute anything.

The task and observations are encoded as JSON with angle brackets escaped
to neutralize Qwen control-marker spellings in untrusted input. This is
not a complete prompt-injection defense; deterministic policy is mandatory.
Only fixed corrections are added for malformed proposals. The entire
current-task transcript stays in the latest user payload, so existing
tokenizer context checking rejects oversized requests instead of dropping
observations. The default 2048-token context may be insufficient for all
tool descriptions and several observations; later verification must
evaluate a larger context or compact tool selection. The host must charge
actual tokens or full reservation on unknown failures, set each call budget,
enforce deadlines and manage retries. Missing scopes require a separate
trusted user-picker flow. No ordinary conversation history is imported.

Four ordinary regression cases are authored, not executed. No dependencies
or model downloads were added. Actual Qwen reliability and template
compatibility remain unverified. This is not native Qwen tool calling.

## Part 08 — bounded execution runtime

The provider-independent synchronous run_task loop coordinates model decisions,
strict parsing, budgets, task transitions, policy decisions and authorized
native dispatch through a trusted RuntimeHost interface. The host must run it
on a dedicated task worker, reserve the desktop operation for the whole task
and bridge the existing inference worker and NativeTools. No policy lock is
held across inference, approval or native execution. Persistence callbacks
must commit atomically and not reenter policy; audit flushing serializes under
the policy lock and retains failed batches for recovery. Omission counts are
persisted and cause the current task to fail closed.

The runtime journals task start, transitions, proposed/rejected tools,
dispatch intent, actual outcomes and terminal state. Before dispatch it
requires durable consumed-authorization audit and dispatch-intent records;
failed persistence stops execution. A permit is consumed once. The report
keeps known execution outcomes even if the result journal fails. An unresolved
dispatch intent is uncertain after interruption, never an instruction to
replay. The future store must use session namespace plus audit sequence for
idempotence and record terminal recovery. Raw proposals, paths, contents
and observations are not persisted by this interface; transient UI callbacks
may display authorized data. A model-written final answer is not evidence of
successful native execution; display authoritative execution records separately.

Tool attempts are counted before authorization. Malformed proposals consume
the existing retry budget; model invocation failures, denied operations and
tool failures end the task instead of retrying automatically. One launch
attempt at most is allowed per task, even for different application IDs.
Launch errors are conservatively uncertain; known dispatch is recorded before
cancellation/timeout checks. No missing scope can be granted by the model:
the user must select a resource through a separate trusted picker and start a
new task. Initial scope hints are a snapshot; the policy checks live grants
at every authorization and dispatch. A deadline-limited cancellation child
lets existing inference checks stop at the task deadline without changing
ordinary chat behavior. Host callbacks and native blocking calls must
cooperate; no forced termination or background work is added.

Nine model-free regression cases are authored, not executed: successful
ordering, malformed proposals, tool budgets, denial, persistence failures,
launch cancellation/uncertainty/replay, deadline cancellation and retained
audit records. No native actions are invoked by these fixtures.

SQLite persistence and desktop/model-worker bridge remain pending. A runtime
report with journal_incomplete is not durable verification. No dependencies
were added; compilation and real execution remain unverified.

## Part 09 — SQLite agent journal

Schema version 2 adds agent_tasks, agent_task_events, agent_executions,
permission_audit and permission_audit_batches. Fresh databases apply both
migrations; existing v1 databases apply only 002_agent.sql. Migration and
startup recovery share the existing exclusive transaction. Chat history,
model references and preferences are preserved. Newer versions fail closed.

The core AgentStore contract is implemented by SqliteStore. The host must
allocate an ID before run_task, serialize database access through the same
existing store, and call interrupt_agent_task for an unfinished allocation
on startup/runtime errors. One unfinished agent task is permitted; chat generation and task allocation
also reject a conflicting active operation. Events,
state transitions, dispatch intents and outcomes are transactional. The
store requires a matching consumed-authorization record for each intent.
An outcome must match an existing pending intent for the same task/tool;
finished tasks reject later events. Recovery finalizes unfinished tasks
as interrupted, marks unresolved intents uncertain, and never replays them.
Known outcomes are preserved. The history API is bounded and paginated by
monotonic task ID. Agent task ID transport must be lossless in the UI.

Audit records are keyed by session namespace and sequence; exact retries
are idempotent, conflicting retries fail. Nonempty batches persist omitted
counts atomically; an impossible empty batch with omissions fails closed.
No grants or approvals are restored. No task prompts, answers, raw
arguments, private paths or document contents are saved in this journal
by design. History shows IDs, states, timestamps, usage and execution/audit
metadata only. SQLite remains unencrypted and is not a tamper-proof log.
Seven regression cases are authored, not executed. No new packages were added. Actual migration, recovery, disk durability,
compilation and runtime behavior remain unverified.

## Part 10 — Tauri commands and events

The desktop host now owns one session-scoped AgentSession with a fresh
random permission namespace, the platform-supported tool registry, native
resource handles, and one active task/approval broker. The agent task runs
on a dedicated Rust thread while all model decisions use the existing
InferenceWorker; ordinary chat, model loading/unloading and settings share
one atomic operation lease and cannot overlap agent execution.

The Tauri bridge exposes start/cancel, exact-operation approval, paginated
task history, task detail, native file/directory selection, scope listing
and immediate scope revocation. File and directory grants are created only
after the native picker returns and are bound to the existing Linux
openat2-backed FilesystemScopes registry. Revocation intentionally bypasses
the global operation lease so it can invalidate an in-flight permit.
Application approval displays the trusted catalog name, desktop entry and
executable; dispatch revalidates the catalog snapshot again in agent-tools.

Agent progress is emitted on the app event channel as agent-event. Events
cover task start/state, proposals, approval requests, dispatch, execution
outcomes, transient observations and final reports. Raw proposals and
observations remain out of SQLite. Task IDs are returned to the UI as
decimal strings, including IDs nested in stored event/audit payloads, to
avoid JavaScript integer precision loss. The frontend listener and user
interface are Part 11.

Shutdown cancels the active task, wakes any approval wait, and joins the
task thread before releasing the inference worker. Unfinished task rows are
recovered as interrupted, and unresolved dispatches remain uncertain.
Session grants and approvals are never restored. The command capability is
restricted to the main window; event listening is explicitly permitted.
The agent-tools crate was already a workspace member; the desktop package
now depends on it directly. No third-party dependencies were added.

Part 10 source implementation is committed but unverified. No build, test,
format, or runtime command was executed. The existing agent journal and
native adapters have not been compiled together through this bridge. UI
approval rendering, event consumption, and live desktop behavior remain
pending Part 11 and final static integration review.

## Part 11 — desktop Agent UI

The desktop frontend now includes a persistent Chat/Agent mode switch while
keeping the existing conversation view mounted and unchanged. Agent mode
subscribes to the app-scoped agent-event channel for the lifetime of the
workspace; the listener is released on cleanup, and task start waits for
listener registration so early progress and approval events are not lost.

The Agent workspace provides a task composer, start/cancel controls, live
task-state and tool-execution timeline, transient tool-result inspection,
final answer/error and budget presentation, task history/detail, execution
metadata and permission-audit history. Task IDs remain decimal strings.
Task history intentionally contains metadata only: prompts, answers and raw
observations are not persisted by the journal.

Application-launch approvals use a modal showing the trusted desktop
application name, desktop entry and executable. The user can approve that
exact operation once or deny it; closing the dialog denies the request.
If trusted target details are unavailable, approval is disabled. Pending
approvals remain visible when switching back to Chat.

The permission manager uses native file and folder pickers, lists active,
expired and revoked scopes, and supports immediate revocation during an
active agent task. The UI discloses the one-hour lifetime and that grants
are not restored after restart. The backend remains authoritative for all
permission checks; the UI is not an authorization boundary.

The Chat view remains mounted when switching modes. Agent execution blocks
new chat/model operations at the UI and backend operation gates. Existing
chat controls, model dialogs and preferences are reused. No dependencies
were added.

Part 11 source implementation is committed but unverified. No build, test,
typecheck, formatter or runtime command was executed. Event serialization,
React typing, native dialog behavior and the end-to-end approval flow remain
unverified until the later integration-review milestone.

## Implementation sequence

| Part | Scope | Status |
| --- | --- | --- |
| 00 | Pinned baseline review | Source reviewed |
| 01 | Domain, lifecycle, errors, budgets | Implemented; unverified |
| 02 | Tool registry and validation | Implemented; unverified |
| 03 | Permission engine | Implemented; unverified |
| 04 | Calculator and device tools | Implemented; unverified |
| 05 | Linux filesystem tools | Implemented; unverified |
| 06 | Linux application tools | Implemented; unverified |
| 07 | Model decision protocol | Implemented; unverified |
| 08 | Bounded execution runtime | Implemented; unverified |
| 09 | SQLite task persistence | Implemented; unverified |
| 10 | Tauri commands and events | Implemented; unverified |
| 11 | Desktop agent UI | Implemented; unverified |
| 12 | Static integration review and handoff | Pending |

The Linux adapters, bounded runtime, SQLite journal, Tauri host bridge and desktop Agent UI now exist as source. Final static integration review remains pending.
