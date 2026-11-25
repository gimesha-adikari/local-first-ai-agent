# v0.1.0 release notes

v0.1.0 is the first accepted Linux CPU release candidate with a built-in
offline assistant and bounded Agent tasks. Publication is pending.

## What's included

- **Offline Chat** with streaming, saved multi-turn conversations, Stop,
  Regenerate and eligible reply continuation. New profiles target
  8,192 context tokens, 4,096 output tokens and four CPU threads with
  Thinking off. Existing saved preferences carry forward.
- **Agent tools** for calculations, device information, read-only file
  search and reads, and trusted desktop application discovery and launch.
  Files and folders require explicit grants. Launches require one-time
  approval of the exact target.
- **PDF handling** with suffix-filtered search, page ranges and coverage
  warnings. The native preview and model summary are displayed separately.
  The final candidate passed the Qwen exact-file page-5 regression.
- **Local continuity** with saved Chat and Agent history, task cancellation,
  and conversation deletion. SQLite remains unencrypted and raw tool
  previews end with the session.

## Install and choose a model

The Linux `amd64` Debian candidate includes **LFM2.5-1.2B-Instruct Q4_K_M**
for lightweight Chat. Thinking and Agent are disabled with this model.
The **optional Qwen3.5-4B GGUF** supports Chat, Thinking and Agent and is
imported through Models. The model-free executable is available as a
source-build option. [Installation, checksum and model options](../README.md#install-on-linux)

## Known limitations

CPU inference can take minutes for multi-step Agent tasks. Task time
defaults to 300 seconds, and launch approval lasts up to 60 seconds within
the task budget. A launch that has dispatched can still open after
cancellation. PDF reads require selectable text, Poppler and Linux
namespace isolation. Each read covers at most four pages. v0.1.0 has no
OCR, GPU inference, or support claim for platforms outside the verified
Linux target. [Full limits and privacy details](../README.md#known-limitations)

The original application source is Apache-2.0. The bundled weights
retain the LFM Open License v1.0 and its commercial-use restrictions.
[Application license](../LICENSE) · [Bundled model notice](../third_party/model-licenses/LFM2.5-1.2B-Instruct/NOTICE.md)

[Task 05 desktop acceptance](task-05-integration-report.md) covers the
earlier package. [Task 05-A exact-file verification](task-05-a-exact-file-report.md)
supersedes its exact-file result. [Distribution notice verification](task-07-a-distribution-notices.md)
identifies the replacement Debian package.
