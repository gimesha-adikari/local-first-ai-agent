# v0.1.0 Linux Agent verification

Verification used the Linux x86-64 checkout at
`18d9bb8ac302c297acf0aaf671ea93631d8b4914` with the local verification
patches. The machine ran Ubuntu 26.04.1 on an Intel Core i5-8500 with 14 GiB
of RAM. Native builds used one low-priority job pinned to one CPU. Model
inference used four CPU threads, no GPU layers, and a 4096-token context.
The existing `Qwen3-0.6B-Q4_K_M.gguf` matched SHA-256
`b0638f08417a2d3c8652760462eb5407c6e30173cf9608ad0820757a281eea0e`.
All prompts, files, and SQLite databases used for verification were synthetic.

## IPC and application approval

The agent event tag keeps its snake-case event names. Rust now sends
camel-case variant fields such as `taskId`, `operationId`, `expiresInMs`,
and `journalIncomplete`. Snapshot tests cover all ten variants and the nested
approval, budget, error and execution data. History and command tests check a
decimal-string task ID above JavaScript's safe integer range. IPC inputs use
the existing string task and operation identifiers. Persisted event data
continues to use its existing schema.

The Linux desktop IPC fixture substitutes only the inference provider. It
uses the real Tauri commands, WebView, permission policy, SQLite journal, GIO
catalog, and native launcher. The fixture selected KCalc by name from the
actual `apps.list` result and sent the returned opaque ID. The catalog is
session-scoped and resolves that ID to the stored immutable candidate. The
approval broker revalidates the target before showing it. The executor
revalidates it again immediately before dispatch.

| Check | Observed result |
| --- | --- |
| Start, planning and execution events | Displayed live for the fixture calculator request. |
| Tool result and terminal event | The result event arrived and the desktop displayed `The calculator returned 14.` |
| Cancellation | The task reached `Cancelled` and the composer accepted another task in the same session. |
| Exact approval target | Dialog showed `KCalc`, `/usr/share/applications/org.kde.kcalc.desktop`, and `/usr/bin/kcalc`. |
| Deny | Task reached `Denied`. SQLite recorded only discovery and the test X display had no KCalc window. |
| Approve once | SQLite recorded one consumed launch authorization and one successful launch execution. A KCalc window appeared in the isolated display. |
| Expired and reused approval | An unanswered desktop approval reached `Timed Out` after about 120 seconds with no launch. Policy tests rejected expired approvals, replay, and different arguments. |
| Uncertain and cancelled dispatch | The deterministic runtime tests preserved known dispatch on cancellation and reported an ambiguous launch failure as uncertain without retrying. |

The approval test blocked IPv4, IPv6 and packet socket creation with an
inherited seccomp filter and allowed Unix-domain sockets for X11 and D-Bus.
Running trusted discovery inside an unprivileged user namespace would map
host root-owned files to an unmapped UID and hide eligible entries. The
approval run kept host ownership visible and used a private Xvfb display,
D-Bus session, and application data directory.

## Model decision investigation

The baseline rendered the embedded template with system and user roles and
an empty `<think>` block before the assistant response. The user payload
contained the task and a list of prior steps. Each prior result includes
the exact validated arguments and the typed, untrusted tool observation.
The system role contained the registered JSON schemas for all six tools.

On five initial synthetic requests, the baseline used 701–710 prompt tokens
and reserved 512. It returned valid final-decision JSON for all five without
requesting a tool. It gave an incorrect product for 137 × 89 and invented
file names and a fixture code. In a separate follow-up with a real-shaped
synthetic observation, it returned the correct `12193`. A first revision
with tool examples used 916–925 prompt tokens and still produced premature
final decisions. Those cases ended normally and were well within the
4096-token context. The desktop runtime permits up to 512 output tokens per
decision, 2,048 output tokens per task, eight model calls, and two retries.
A malformed decision still consumes the retry budget and never dispatches.

The prompt now includes concrete registered-tool examples and a short
instruction after the task data that distinguishes opening decisions from
decisions with observations. Without constrained decoding, this revision
produced Markdown fences, missing scope fields, and tool names in the `kind`
field. The observed prompts used 951–960 tokens, or 995 after the calculator
observation. The strict parser rejected those outputs. The agent now uses
a llama.cpp grammar for the existing JSON envelope. The grammar constrains
the output shape and leaves tool, argument and permission validation to the
existing checks. Chat sampling retains its existing path. The duplicate
sampler acceptance was removed because llama.cpp accepts during sampling.
With the grammar, the synthetic calculator response parsed as a tool call
with `137*89`. The file search and read responses selected the expected
tool names but still omitted `scope_id` and failed argument validation. A
synthetic device prompt with a directory scope present returned `device.info`
as a final answer. The two desktop device prompts before any grant produced
valid `device.info` calls. These results distinguish envelope defects from
remaining model choices about the tool, its arguments, and the final answer.

The runtime also suppresses consecutive,
identical successful read-only calls. The model sees the previous result and
a correction, and a repeat consumes the existing retry budget. Distinct
calls and calls after another successful operation retain their normal
validation and authorization path. Launch operations keep their separate
single-attempt rule. Diagnostics print only fixed synthetic fixtures in an
ignored test. Production logging and audit retention remain unchanged.

The actual embedded GGUF template has a `tools` branch and `<tool_call>`
tags. This app renders its ordinary chat branch and uses its own strict JSON
decision envelope. Qwen recommends the Hermes-style format for its native
function calling. Native tool messages would require separate rendering
and parsing work and a fresh acceptance run through the same validator and
permission policy. Native tool calling was inspected and was not run here.
[Qwen function calling documentation](https://qwen.readthedocs.io/en/latest/framework/function_call.html)

## Offline natural-language desktop trials

The final release ran two prompts per tool category using the existing Qwen
model. No prompt included an exact tool-call envelope. Calculator, device,
and file trials ran in a network namespace with loopback down. Application
discovery ran under the socket filter so it could still see root-owned
desktop entries. The file cases used a native picker grant for a temporary
directory containing `ledger.txt` and `budget_ledger.txt`. The expected codes
were `ORBIT-3795` and `LIME-2407`, respectively.

The tool column lists validated calls and durable execution outcomes. Invalid
arguments were rejected before dispatch. Extra calls count native attempts
beyond the expected operation, including executions of unrelated tools. The
model/retry figures come from SQLite. `Live` means the screen showed the
terminal state and the composer recovered without restarting the app. The
terminal state alone is insufficient to establish answer correctness.

| Request | Tool and arguments | Extra native calls | Model calls / retries | Answer | Terminal / UI |
| --- | --- | ---: | ---: | --- | --- |
| Use the local calculator to work out 137 times 89. Report the result. | `math.calculate`, accepted, succeeded | 0 | 2 / 0 | `12193`, correct | Completed / live |
| What is 19 times the sum of 23 and 7? Use the calculator. | `math.calculate`, accepted, succeeded | 0 | 2 / 0 | `444`, expected `570` | Completed / live |
| Inspect this device using the available tool. What operating system is it running? | `device.info`, accepted, succeeded | 0 | 2 / 0 | Linux, correct | Completed / live |
| Use device information to report how many logical CPUs this device has. | `device.info`, accepted, succeeded | 0 | 2 / 0 | Six logical CPUs, correct | Completed / live |
| Find text files whose names contain ledger in the authorized directory. | Invalid arguments, no dispatch | 0 | 3 / 2 | No final answer | Failed / live |
| Search the authorized directory for filenames containing budget and tell me the matching filename. | Invalid arguments, no dispatch | 0 | 3 / 2 | No final answer | Failed / live |
| Read ledger.txt from the authorized directory and report its fixture code. | Invalid arguments, no dispatch | 0 | 3 / 2 | No final answer | Failed / live |
| Open budget_ledger.txt in the authorized directory and tell me the code inside. | Unrelated `device.info` call, accepted and succeeded | 1 | 2 / 0 | `123456`, expected `LIME-2407` | Completed / live |
| List the locally installed desktop applications using the discovery tool. | `apps.list`, accepted, succeeded | 0 | 2 / 0 | Displayed opaque IDs without application names. Incomplete. | Completed / live |
| Discover what desktop apps are available. Is KCalc on the list? Do not launch anything. | `apps.list`, accepted, succeeded | 0 | 2 / 0 | KCalc is available, correct. No launch request. | Completed / live |

Six of ten trials dispatched the intended tool and four produced a verified
correct final answer. The desktop rendered the terminal event in all ten.
Two supplemental repeats reproduced the compound arithmetic and discovery
failures. Both reached `Completed` with live updates, one successful native
call, two model calls, and zero retries. In the arithmetic repeat, the
transient native result and the final answer both contained `444`, placing
the mismatch at the tool boundary. The exact expression from that desktop
task was not persisted. The discovery repeat displayed the full response
and confirmed that it contained only opaque IDs.

The 0.6B model still supplied invalid file arguments, picked an unrelated
tool for one file request, and gave a wrong arithmetic result. Permissions
protect scope and dispatch. They do not establish whether a model chose a
relevant tool or faithfully reported its result. Qwen3-0.6B failed this
agent acceptance matrix.

The follow-up below uses the locally available Qwen3.5 artifact and the same
application protocol, prompts, context, and CPU thread count.

## Engineering checks

| Check | Result |
| --- | --- |
| Rust formatting and `git diff --check` | Passed |
| Release-profile Clippy, all targets, warnings denied | Passed |
| Full Rust workspace with Tauri custom protocol | 82 passed, 8 opt-in tests skipped by default |
| Frontend tests | 8 passed |
| TypeScript and Vite production build | Passed |
| Linux Tauri release | Passed, produced `target/release/local-first-ai-agent` |
| Opt-in pinned-model inference lifecycle | Passed after constrained agent decoding was added |

The ignored `qwen_agent_decision_diagnostics` test reads `AGENT_TEST_MODEL`.
The ignored `desktop_acceptance::desktop_approval_fixture` test requires
X11 and an isolated temporary `AGENT_ACCEPTANCE_PROFILE` whose `data`
subdirectory is set as `XDG_DATA_HOME`. Build that fixture with
`cargo test --release --workspace --locked --features tauri/custom-protocol --no-run`.
Run its printed desktop test executable with
`--ignored --exact desktop_acceptance::desktop_approval_fixture`. Enter
`Fixture calculator`, `Fixture cancellation`, and `Fixture KCalc approval`
in the Agent workspace. Deny the first launch and approve the next in the
isolated display. Verify the native window and the journal execution count.

## Qwen3.5-4B comparison

The existing `Qwen3.5-4B-UD-Q4_K_XL.gguf` is 2,912,109,728 bytes. Its
SHA-256 `b252c5610a42ca82d20fe2a12813e9d069eed89292907e26c783eeb0bc961bc7`
matches Unsloth's publication at file revision
`6727efff692673b7a2ebb4c6ba830ce4f5fb1309`. The Apache-2.0 GGUF v3 file
identifies the base model as `Qwen/Qwen3.5-4B`, architecture `qwen35`,
and tokenizer preprocessor `qwen35` with a 248,320-entry GPT-2 BPE
vocabulary. The advertised `UD-Q4_K_XL` quantization is mixed across 426
tensors. It contains 74 Q4_K, 69 Q5_K, 24 Q6_K, 24 Q8_0, 10 IQ4_XS,
177 F32 and 48 F16 tensors. The GGUF metadata identifies the base model
repository. A base-checkpoint commit is unavailable in the embedded metadata.
[Published artifact and checksum](https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/blob/main/Qwen3.5-4B-UD-Q4_K_XL.gguf)
 · [Upload revision](https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/commit/6727efff692673b7a2ebb4c6ba830ce4f5fb1309)

The pinned llama.cpp revision `d81235049384534c167caea52b85a694f6103d14`
loaded the model as `qwen35` with 4,205,751,296 parameters and a metadata
context limit of 262,144. The embedded text template rendered both thinking
modes. Disabled thinking adds an empty `<think>` block, while enabled
thinking ends with an open `<think>` tag. llama.cpp identified `<|im_end|>`
(248046) and `<|endoftext|>` (248044) as end-of-generation tokens. The
`<|im_start|>` token and the two thinking delimiters were ordinary control
tokens outside that end-of-generation set. The existing CPU model loader,
template renderer, and JSON grammar path operated with the new architecture
without a production compatibility patch or dependency upgrade.

### CPU profile and calculator investigation

The ignored `local_agent_model_profile` test uses two fixed calculator
prompts with a 4096-token context, four CPU threads, thinking disabled for
decisions, and a 512-token reserved output budget. It logs the exact
synthetic model proposal and real worker statistics. The separate
`local_qwen35_native_tool_format` test supplies a single synthetic tool to
the native template. Its output is isolated from the JSON comparison and
the test never dispatches a tool.

In the first Qwen3.5 profile run, the API loaded the model in 144.98 seconds
while the host experienced heavy I/O and swap activity. The next desktop load
took 3.63 seconds from clicking Load to model ready. The OS file cache was
left intact. In the profile, the compound and simple decisions each used 959
prompt tokens. Time to first token was 34.09 and 24.79 seconds, prompt
throughput was 29.35 and 38.89 tokens/s, and decode throughput was 4.70 and
5.10 tokens/s. The process peaked at 4,094,848 KiB RSS. Host `MemAvailable`
started at 5,298,216 KiB and ended at 8,085,440 KiB. Host swap counters rose
by 530,600 pages in and 870,396 pages out during the profile. Those counters
include other host processes. A first-chunk cancellation returned after
14.12 ms in this test.

The 0.6B profile reproduced the compound desktop prompt with no scopes in
context: “What is 19 times the sum of 23 and 7? Use the calculator.” The
constrained model response proposed `math.calculate` with the exact
expression `19*23+7`. Normal precedence evaluates it to `444`, which matches
the previous desktop's native result and final answer. Qwen3.5 instead
proposed `19*(23+7)` and the desktop returned `570`. The deterministic native
regression test checks both forms. The discrepancy comes from expression
generation and requires no change to calculator precedence or evaluation.

| CPU profile measure | Qwen3-0.6B Q4_K_M | Qwen3.5-4B UD-Q4_K_XL |
| --- | ---: | ---: |
| Engine / API wall load | 4.578 / 6.405 s | 144.980 / 144.980 s under heavy host I/O |
| Prompt tokens per measured decision | 946 | 959 |
| First token, compound / simple | 3.754 / 3.642 s | 34.091 / 24.793 s |
| Prompt tokens/s, compound / simple | 270.21 / 277.85 | 29.35 / 38.89 |
| Decode tokens/s, compound / simple | 17.31 / 16.55 | 4.70 / 5.10 |
| Process peak RSS | 1,205,544 KiB | 4,094,848 KiB |
| Host `MemAvailable` before / after | 6,812,100 / 6,787,332 KiB | 5,298,216 / 8,085,440 KiB |
| Host swap pages in / out during profile | 1,198 / 0 | 530,600 / 870,396 |
| First-chunk cancellation until return | 26.92 ms | 14.12 ms |

These are serial synthetic runs under the observed host load. Disk cache was
left intact. The slower cold-pressure load and the later warm desktop loads
are separate observations.

### Qwen3.5 desktop trials with the existing JSON grammar

The initial eight cases used a fresh desktop profile in a Linux network
namespace with loopback down. The directory grant came from the native
picker. The Rust runtime supplied the authorized scope identifier in the
model context and checked it at dispatch. The prompts and synthetic file
contents matched the Qwen3 baseline above. All eight calls passed typed
argument validation, dispatched the indicated tool successfully, and
displayed the result and terminal event live.

| # | Trial | Accepted tool | Extra native calls | Model calls / retries | Observed answer | Terminal / UI | UI latency |
| --- | --- | --- | ---: | ---: | --- | --- | ---: |
| 1 | 137 × 89 | `math.calculate` | 0 | 2 / 0 | `12193`, correct | Completed / live | 58.87 s |
| 2 | 19 × (23 + 7) | `math.calculate` | 0 | 2 / 0 | `570`, correct | Completed / live | 57.29 s |
| 3 | Device operating system | `device.info` | 0 | 2 / 0 | Linux, correct | Completed / live | 60.45 s |
| 4 | Logical CPU count | `device.info` | 0 | 2 / 0 | Six, correct | Completed / live | 61.78 s |
| 5 | `.txt` files containing `ledger` | `files.search` | 0 | 2 / 0 | Both fixture filenames, correct | Completed / live | 84.33 s |
| 6 | Files containing `budget` | `files.search` | 0 | 2 / 0 | `budget_ledger.txt`, correct | Completed / live | 69.16 s |
| 7 | Read `ledger.txt` | `files.read` | 0 | 2 / 0 | `ORBIT-3795`, correct | Completed / live | 69.10 s |
| 8 | Read `budget_ledger.txt` | `files.read` | 0 | 2 / 0 | `LIME-2407`, correct | Completed / live | 71.32 s |
| 9 | List installed desktop applications | `apps.list` | 0 | 2 / 0 | All 45 names in the trusted catalog, exact sequence | Completed / live | 105.92 s |
| 10 | Is KCalc available? | `apps.list` | 0 | 2 / 0 | KCalc, with its actual catalog ID | Completed / live | 78.86 s |

These desktop latencies run from clicking Start task to the observed terminal
history update. Across the initial eight, the desktop process reached
4,337,092 KiB RSS. The lowest sampled system `MemAvailable` was 5,414,156
KiB. System swap counters increased by 85,182 pages in and 19,530 pages
out. The process stayed active for all eight tasks before closing the
profile and reading the execution journal.

Application discovery used the Unix-domain-sockets-only isolation described
in the approval section so the trusted root ownership checks could operate.
The app loaded the model in 1.61 seconds on this warm run, peaked at
4,318,152 KiB RSS and retained at least 5,488,300 KiB of sampled system
`MemAvailable`. The two discovery tasks added 15,174 pages to swap-ins
and 115 to swap-outs on the host. The final answer for trial 9 matched the
expanded native observation's 45 display names in order. Trial 10 found
KCalc in the observed catalog. These trials dispatched no launches.

Across the ten scenarios, Qwen3.5 selected the intended tool 10/10 times
and returned a verified correct answer 10/10 times. Qwen3-0.6B scored 6/10
and 4/10 on the same matrix. All ten Qwen3.5 tasks used two model calls,
zero retries, and one native dispatch. The desktop rendered every terminal
state live. The larger model's median UI latency was 69.13 seconds, with
a range of 57.29–105.92 seconds against the unchanged 120-second deadline.
On the common SQLite creation-to-terminal metric, Qwen3-0.6B had a
15.30-second median and Qwen3.5 had a 68.67-second median. Audit records
confirmed all four file operations consumed authorization for the same
picker-granted scope. The application used the existing grammar and did
not receive hand-written JSON or a scope ID in any of the ten user prompts.

### Qwen3.5 native launch approval and deadline

Two additional natural-language tasks asked for KCalc through application
discovery and approval. The native dialog showed the trusted KCalc name,
desktop entry, and executable in the private test display.

| Decision | Execution and UI evidence | Model calls / retries | Terminal task / elapsed |
| --- | --- | ---: | --- |
| Deny | One successful discovery, zero launch dispatches, zero KCalc windows | 2 / 0 | Denied / 76.52 s |
| Approve once | One successful discovery, one successful launch record, a KCalc window visible | 3 / 0 | Timed Out / 122.04 s |

The approval route worked. The approved task reached the deadline during
its third model call while preparing a final response. The durable execution
journal preserved the completed launch and the desktop displayed the timeout
live. The timeout applied to the final response. The journal recorded one
successful launch attempt.

The approved operation had exactly one consumed launch authorization. The
denied operation had a `user_rejected` audit record and no consumed launch
authorization. SQLite integrity returned `ok` and its foreign-key check was
empty after the desktop runs.

During the two approval tasks the desktop process peaked at 4,315,704 KiB
RSS. Sampled host `MemAvailable` reached a minimum of 4,033,936 KiB. Host
swap counters advanced by 129,605 pages in and 226,369 pages out.

In a separate live cancellation trial, the user clicked Cancel immediately
after the task started. The desktop showed `Cancelled` after 14.352 seconds,
with one model call, zero emitted decision tokens, zero tool calls and no
retries. This was a prompt-stage UI measurement. The first-chunk profile
measurement of 14.12 ms covers a different cancellation point. The
cancellation-run model load took 13.94 seconds and host swap counters
increased by 28,895 pages in and 159,182 pages out. The click-to-terminal
delay is a remaining responsiveness limitation on this machine.

### Native Qwen tool format probe

In a separate offline process, the embedded Qwen3.5 template received one
synthetic calculator tool definition. The model emitted a native
`<tool_call>` with `<function=math.calculate>` and an `expression` parameter
of `137 * 89`, then reached end of generation. It used 273 prompt tokens,
32 output tokens, an 8.293-second first-token time, and 6.77 decode tokens/s.
This probe measured the native envelope with a single-tool prompt. No tool
was dispatched in the probe. The ten desktop trials and the approval flow
used the application's existing multi-tool constrained JSON protocol.

### Regression checks

Formatting, workspace Clippy with warnings denied, workflow lint,
TypeScript, Vite, and the Linux Tauri release build passed. The workspace
ran 83 tests successfully and skipped 10 opt-in tests by default. All eight
frontend tests passed. The rebuilt release executable matched the one used
for Qwen3.5 desktop acceptance byte for byte. The pinned Qwen3-0.6B model
passed the separate profile, CPU inference lifecycle, and retained-context
tests with networking disabled.
## Final stabilization

The earlier ten-trial run took 57.29–105.92 seconds per task. An approved
launch used a third model call and crossed the earlier 120-second task
deadline after recording one successful dispatch. The first stabilization
smoke test also found a separate 120-second task-lease cap in the permission
engine. The task budget and permission engine now share a 600-second
ceiling. The default is 300 seconds, the desktop accepts 1–600 seconds per
task, and human confirmation may use up to 60 seconds of the remaining
task time. Approval, operation, token and retry accounting still run in
the core and host. The elapsed and remaining displays are informational.

The cancel action reaches the active task's shared flag. Its inference
view also has the task deadline, and the same flag reaches the approval
wait and native tool permits. The pinned llama.cpp header exposes a CPU
abort callback, but `llama-cpp-2` 0.1.161 keeps the native handle private.
Agent inference checks cancellation before context creation, between
16-token prefill batches, and between decode calls. Chat keeps its
64-token batches. Context creation, a native decode in progress, and
blocking filesystem calls remain cooperative boundaries. The task thread
publishes its terminal event after releasing the operation lease.

The final desktop acceptance uses the actual Linux release without a Vite
dev server, separate temporary XDG profiles, and synthetic files. Chat,
calculator, device, and file scenarios use a user/network namespace with
loopback down. Application discovery and launch use an inherited seccomp
filter that rejects non-Unix sockets while preserving the system ownership
needed for desktop-entry trust. Approval waits stay interactive in the
private Xvfb display. The native KCalc window and SQLite execution records
provide distinct evidence for an approved launch.

### Stabilization engineering checks

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` and `git diff --check` | Passed |
| Locked release Clippy, all workspace targets, Tauri custom protocol, warnings denied | Passed |
| Locked release workspace tests with Tauri custom protocol | 84 passed, 11 opt-in tests ignored during this run |
| Frontend tests | 8 passed |
| TypeScript check and Vite production build | Passed |
| Linux Tauri release, with embedded assets | Passed. Executable `target/release/local-first-ai-agent` |

Native builds ran serially with CPU 0 affinity, `nice -n 15`, idle I/O
priority, and Cargo offline. The release reused the existing build cache.
The root filesystem had about 1.9 GiB available at the start and 1.7 GiB
available after the release build. No runtime or build dependency versions
changed during stabilization.

### Task timer and deterministic desktop checks

- The running release displayed a `0:36` elapsed timer with `4:24`
  remaining on the 300-second budget. A later task independently displayed
  `0:21` elapsed and `4:39` remaining.
- The test desktop, built with the same IPC and a deterministic decision
  provider, disabled Start task at 601 seconds. A one-second setting
  timed out in 1.58 seconds from the Start click to the terminal UI
  check. The task dispatched no tools. The following calculator task
  completed successfully.
- In the deterministic launch fixture, cancellation from the visible
  KCalc approval dialog took 736 ms and 499 ms on two runs. One
  discovery had completed in each. SQLite contained zero launch
  executions, and no KCalc window opened. Two unanswered approvals
  expired after 60.4 and 60.1 seconds, each with zero launch executions
  and no KCalc window.
- `repeated_launch_is_blocked_and_journal_failure_preserves_result` and
  `launch_cancellation_preserves_dispatch_and_uncertainty_is_not_retried`
  passed in the Rust workspace. They force repeat and uncertain side-effect
  paths with exactly one dispatch each.

### Qwen3.5-4B desktop rerun

The rerun used the same ten prompts, synthetic file contents, constrained
JSON grammar, 4096-token context, four CPU threads and disabled thinking.
All ten tasks completed live with exactly one successful dispatch,
two model calls and zero retries per task. Successful dispatch establishes
that arguments passed typed validation and any required scope check.

| Trial | Tool | Observed correct answer | Click to terminal UI |
| --- | --- | --- | ---: |
| 137 × 89 | `math.calculate` | 12193 | 81.60 s |
| 19 × (23 + 7) | `math.calculate` | 570 | 71.07 s |
| Operating system | `device.info` | Linux | 82.59 s |
| Logical CPU count | `device.info` | Six | 71.47 s |
| Find text files containing `ledger` | `files.search` | `budget_ledger.txt`, `ledger.txt` | 86.67 s |
| Find filenames containing `budget` | `files.search` | `budget_ledger.txt` | 82.69 s |
| Read `ledger.txt` | `files.read` | `ORBIT-3795` | 85.44 s |
| Read `budget_ledger.txt` | `files.read` | `LIME-2407` | 85.36 s |
| List installed applications | `apps.list` | All 45 trusted catalog entries, in order | 127.16 s |
| Find KCalc without launching it | `apps.list` | KCalc and its session catalog ID | 95.51 s |

The first eight tasks ran in one profile after a 15.14-second model load.
The desktop process peaked at 4,316,356 KiB RSS. Sampled host
`MemAvailable` stayed at or above 4,742,980 KiB. Host-wide swap counters
increased by 197,258 pages in and 175,495 pages out. The first stabilization
smoke test, before the policy-cap fix, recorded a 167.26-second model load
during heavier memory and I/O pressure.

The two catalog tasks reused the picker-authorized model reference in the
same test profile under the socket filter. The warm load was 1.27 seconds.
The longest trial finished in 127.16 seconds, past the old 120-second cap.
Across all ten, the median click-to-terminal time was 84.03 seconds
(71.07–127.16 seconds). The previous median was 69.13 seconds. The smaller
prefill batch and host resource conditions both changed between runs, so
these elapsed times do not isolate the throughput cost of the batch change.

All four file calls consumed authorization for the same directory chosen
through the native picker. SQLite integrity returned `ok` and the
foreign-key check was empty after the matrix and launch tests.

### Real launch and deadline enforcement

The actual release displayed KCalc, its `/usr/share/applications/org.kde.kcalc.desktop`
entry and its `/usr/bin/kcalc` executable before each confirmation choice.

| Human choice | Native operations and result | Model calls / retries | Terminal UI |
| --- | --- | ---: | --- |
| Deny | One discovery, zero launch executions, no KCalc window | 2 / 0 | `Denied` in 94.94 s |
| Approve once | One discovery, one successful launch execution, one observed KCalc window, and a correct final answer | 3 / 0 | `Completed` in 157.91 s |

SQLite contained exactly one launch execution across the two tasks. The
denied task recorded `user_rejected` and the approved task consumed exactly
one launch authorization. The third model call finished inside the new
budget. The earlier 120-second run had timed out after the launch dispatched.

A separate multi-step prompt asked for 137 × 89 and the operating system.
The release dispatched `math.calculate` followed by `device.info`, one time
each. The final answer gave 12193 and Linux. It completed live in 117.44
seconds with three model calls and zero retries.

On the actual Qwen3.5 release, setting the task timeout to one second
produced `Timed Out` in 1.47 seconds from Start to the terminal UI check.
No native tool ran. The next task in the same process returned `READY`
after restoring the default timeout.

### Cancellation measurements

Two Qwen3.5 desktop cancellations, triggered during the first model call
before any decision tokens were emitted, reached `Cancelled` in 746 ms and
474 ms. These were the first and second decisions after a 1.24-second warm
model load. Both tasks had zero dispatches and zero retries. The previous
desktop prompt-stage trial took 14.352 seconds under different host memory
and I/O conditions. Native calls remain cooperative, so the observed
latencies do not establish a worst-case bound.

The offline Qwen3.5 native generation test set the shared cancellation flag
after the first emitted constrained decision piece. The provider returned
`Cancelled` in 147.38 ms with one generated token. This run processed
960 prompt tokens at 31.63 tokens per second and reached the first token
after 30.50 seconds. The native measurement ends at provider return.
The desktop measurements include IPC, cleanup and terminal UI observation.

Qwen3-0.6B chat reached `Stopped` 426 ms after the Stop click once the
assistant had visible streamed content. The partial answer remained saved.

The read-only cancellation probe used the release, a real picker-granted
directory and a Qwen3.5 `files.search` decision. `strace` inserted a
one-second delay at the Agent thread's `getdents64` exit to make the
syscall observable. Cancel reached the terminal UI in 1,481 ms. SQLite
recorded `files.search` as failed with `cancelled`, and the task did not
run another model call. This is an instrumented syscall-boundary check.

### Offline chat regression

The Linux release passed the Qwen3-0.6B desktop smoke test in a network
namespace. It saved and recalled the synthetic name Miso within one
conversation and returned `UNKNOWN` in a separate conversation. Search,
rename, regeneration, model unload and reload, and same-process recovery
after Stop all passed. Chat history and preferences survived restart.
Reducing context omitted an earlier turn without deleting it. An oversized
new message was rejected, and deleting the test conversation persisted
across restart. The separate chat database passed integrity and
foreign-key checks and contained one cancelled and ten complete messages.

The offline Rust Qwen3-0.6B model profile, real inference lifecycle and
retained-turn context test also passed using the existing local fixture.

### Reproduction

Run the locked checks and build the embedded release from the project root.

```sh
cargo fmt --all --check
cargo clippy --release --workspace --all-targets --locked --features tauri/custom-protocol -- -D warnings
cargo test --release --workspace --locked --features tauri/custom-protocol
npm --prefix apps/desktop test
npm --prefix apps/desktop run typecheck
npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked
```

Use separate temporary XDG data, cache, config, and runtime directories for
desktop acceptance. Run the release in a private X11 display and D-Bus
session. For cases that do not require trusted application discovery,
launch the process under `unshare --user --map-current-user --net` and
verify that loopback is down and `ip route show default` is empty.
Select a local model through the picker and use the prompts above.
For trusted desktop entries, keep host UID mapping and deny non-Unix sockets
with an inherited seccomp filter. Verify the native target before clicking
Approve, and inspect the task and SQLite audit records after closing the app.

The constrained-generation timing case is the ignored
`llama::tests::local_agent_decode_cancellation` test. Build with
`cargo test --release --locked -p inference --features llama --no-run`.
Set `AGENT_TEST_MODEL` to the local GGUF and run the printed inference
unit test executable under the isolated network namespace with
`--ignored --exact llama::tests::local_agent_decode_cancellation --nocapture`.

For the file-tool timing case, use a synthetic picker-granted directory
and run the release under `strace -D -f -e trace=getdents64
-e inject=getdents64:delay_exit=1s:when=1` in the isolated display.
Cancel while the Agent task thread is stopped on its first directory read.
Check the terminal task and the `cancelled` execution error after closing
the profile.
