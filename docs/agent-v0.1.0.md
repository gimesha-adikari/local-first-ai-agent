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

## Implementation sequence

| Part | Scope | Status |
| --- | --- | --- |
| 00 | Pinned baseline review | Source reviewed |
| 01 | Domain, lifecycle, errors, budgets | Implemented; unverified |
| 02 | Tool registry and validation | Pending |
| 03 | Permission engine | Pending |
| 04 | Calculator and device tools | Pending |
| 05 | Linux filesystem tools | Pending |
| 06 | Linux application tools | Pending |
| 07 | Model decision protocol | Pending |
| 08 | Bounded execution runtime | Pending |
| 09 | SQLite task persistence | Pending |
| 10 | Tauri commands and events | Pending |
| 11 | Desktop agent UI | Pending |
| 12 | Static integration review and handoff | Pending |

No tool, policy enforcement, UI integration, or model-driven execution exists
as a result of Part 01. Later parts must connect these contracts before the
agent can perform any action.
