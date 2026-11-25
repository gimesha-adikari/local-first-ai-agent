# Task 05 integration report

**v0.1.0 Linux amd64 release candidate · 26 November 2026**

Release approval is pending the user's third manual review.

## Final corrections

- SQLite seeds recommended settings only when it creates a profile. The target is **8,192 total context tokens, 4,096 output tokens, four CPU threads and Thinking off**. Recommended model rows cap context at trained capacity. On Linux the recommendation steps down to 4,096 or 2,048 when reported available memory leaves less than 2 GiB or 1 GiB above the model file size, respectively. The output cap is half the resulting context. Existing saved model settings and preferences are preserved.
- Settings explains that instructions, user input, conversation history and generated output share context. LFM2.5 1.2B remains the bundled Chat model with Agent and Thinking disabled. Qwen3.5 4B remains the optional model for Agent and Thinking.
- The bundled Debian package now declares `poppler-utils`, `bubblewrap` and `util-linux` for the existing PDF parser and resource isolation.

## Engineering verification

| Check | Result |
| --- | --- |
| `cargo fmt --all` and `cargo fmt --all --check` | Passed. |
| Locked release Clippy, whole workspace and all targets, `tauri/custom-protocol`, `-D warnings` | Passed. |
| `cargo test --release --workspace --locked --features tauri/custom-protocol` | 121 passed, 28 opt-in tests skipped in the ordinary run. All three opt-in native Poppler tests passed separately on synthetic PDFs in a network namespace. |
| Frontend | 41 tests and `tsc --noEmit` passed. Vite production build passed. Fixture markers were absent from the production JS. The 530 kB bundle produced the existing Vite size advisory. |
| SQLite | Migration and database lifecycle regression tests passed. The packaged v4 profile migrated to v5, retained the original Chat and Qwen row, and kept the saved 4,096/333/3 settings and Enter-to-send preference. `integrity_check` returned `ok` and `foreign_key_check` returned no rows. |
| Git | Diff whitespace checks passed. Git tracks no model weights or developer-specific absolute paths. Existing work was retained. |

The final package is `target/release/bundle/deb/local-first-ai-agent_0.1.0_amd64.deb`, **723,566,304 bytes (690.05 MiB)**. SHA-256 is `ae572f80c5cd65df8a4e8b992e0b10901f0eb854ec9e54a495eac3d83de878f6`. It was built once with existing release caches and staged resources, offline, at low priority with one Cargo and CMake job. Available workspace space was 19 GiB before the build.

The packaged LFM GGUF matched SHA-256 `b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5`. Its license and notice matched the repository copies byte-for-byte. WebKitGTK, GTK, Poppler, Bubblewrap and `prlimit` resolved on the test host. APT's install simulation succeeded. PDF extraction requires Linux user and other namespace support and its seccomp filter. The sandbox ran with those requirements on this host.

The **application license is undecided**. The bundled weights carry the separate **LFM Open License v1.0**. Redistribution requires the license text. Commercial use by an entity with US$10 million or more in annual revenue requires a separate agreement, subject to the license's qualified non-profit exception. [Bundled model notice](../third_party/model-licenses/LFM2.5-1.2B-Instruct/NOTICE.md)

## Acceptance

**Packaged desktop with real models.** The candidate ran from the extracted `.deb` on private X11 and D-Bus sessions with only loopback in the network namespace. Profiles and files were disposable. The user's database was never opened.

| Scenario | Observed result |
| --- | --- |
| Fresh launch and Chat | LFM2.5 auto-loaded with 8,192 context, a 4,096 output limit, four threads and Thinking off. It recalled `Iris-47` and `teal` in a follow-up. Agent was disabled with Qwen guidance, and Thinking was unavailable. |
| Existing profile and model switch | A seeded schema-v4 conversation, Qwen model and custom settings survived migration. The bundled row was added behind Qwen. In the fresh profile, the native picker imported the existing Qwen GGUF and switching loaded it at 8,192/4,096/4. Thinking and Agent were available. |
| Calculator and recall | One native calculation returned **12,193**. Qwen repeated the value on follow-up with zero tool executions. |
| PDF-only search | One native search returned only `01-orchard.pdf`, excluding the ZIP and TXT lookalikes. Qwen's separate answer completed in 115 seconds at the default 300-second task limit. The earlier PDF search synthesis timeout did not recur. |
| Six-page PDF and follow-up | One native read returned pages 1–4 of 6. The card warned that two pages were unread. Qwen summarized the four pages, explicitly identified pages 5–6 as unread, then recalled Monday without a second read. The summary turn took 153 seconds. |
| Exact-file read | **Failed.** The picker granted the exact PDF after the folder was revoked. Qwen supplied a relative path with a file scope. The host ended the turn with `scope_kind_mismatch` before dispatch. No page 5 text or answer was returned. |
| Revocation and cancellation | After revocation, Qwen said it could not read page 6 and executed zero tools. Cancelling an active Agent turn saved it as cancelled with zero dispatches. |
| Continuation, restart and deletion | Continue appended to the same Chat attempt after a 24-token cutoff. Stop saved it as cancelled with 44 cumulative tokens. Chat and Agent history reopened after restart. UI deletion removed the eight Agent tasks, their events and executions, and the linked messages. Both Chat threads and model rows remained. SQLite integrity and foreign-key checks passed again. |
| Interface | Settings, model selection and reopened conversations were usable at 800 × 600. Light and Dark were exercised. The live PDF card kept the extracted text, coverage warning and model summary distinct. |

**Synthetic regression evidence.** Workspace tests covered missing and revoked scopes, exact-target and single-use authorization, and cancellation. The native Poppler regressions passed, including exact-file reading with valid arguments. The deterministic WebView IPC fixture passed continuation and Stop. A separate scripted-model desktop fixture used the real KCalc target and native launcher in a private session with network sockets blocked. Denial and expiry recorded zero launches. Approve once recorded one `apps.launch` dispatch and opened KCalc.

**Real-model backend-only evidence.** No separate backend-only model suite ran in Task 05. The real inference results above came from the packaged desktop.

## Remaining failures and release blockers

- **Exact-file Agent acceptance remains open.** Native extraction works with a valid file-scope request. The real Qwen turn supplied incompatible arguments, so the required page-5 read did not complete. No prompt retries, timeout increases or authorization changes were used.
- The PDF search passed once on this host. Repeated model reliability, lower-memory desktop hardware, a clean install on another distribution and real-model launches of desktop applications from the release binary remain unverified. Approval behavior was exercised in the scripted desktop fixture.
- Public release remains pending the third manual review and the independent application-license decision. No commits, pushes, tags or releases were made.

## Third manual review

From the workspace in a Linux graphical session with an X11 or Xwayland display, run the extracted package with a disposable profile. Keep the same profile to check restart and deletion.

```bash
review_dir=$(mktemp -d)
mkdir -m 700 "$review_dir"/{home,data,config,cache,runtime,state}
dpkg-deb -x target/release/bundle/deb/local-first-ai-agent_0.1.0_amd64.deb "$review_dir/package"
run_review() {
  env -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u AT_SPI_BUS_ADDRESS \
    HOME="$review_dir/home" XDG_DATA_HOME="$review_dir/data" \
    XDG_CONFIG_HOME="$review_dir/config" XDG_CACHE_HOME="$review_dir/cache" \
    XDG_RUNTIME_DIR="$review_dir/runtime" XDG_STATE_HOME="$review_dir/state" \
    GDK_BACKEND=x11 GTK_USE_PORTAL=0 unshare -Urn dbus-run-session -- \
    "$review_dir/package/usr/bin/local-first-ai-agent"
}
run_review
```

1. Verify LFM2.5 loads. In **Settings → Advanced settings**, check the context explanation and the 8,192/4,096/4 defaults with Thinking off. Send “Remember my alias is Iris-47 and my color is teal” and ask for both in a follow-up. Open the mode menu and check the Agent guidance. Toggle Light and Dark and shrink the window to about 800 × 600.
2. In **Models → Import model…**, select the existing Qwen3.5-4B GGUF and switch to it. In Agent, ask “What is 137 times 89? Use the calculator.” Ask for the previous result without tools.
3. Grant `target/task-05-evidence/documents` by selecting its folder row and clicking **Open** in the native picker. Ask “Find PDF files in the selected folder.” Verify only `01-orchard.pdf` is listed. Ask for a summary of pages 1–4 only and check the warning for pages 5–6. Ask which day filters are inspected without rereading. Revoke the folder, use **Select a file…** for the PDF, and ask what page 5 says about a spare pump. Revoke the file afterward.
4. Start and cancel an Agent task. In Chat, use **Response options** to set a one-turn 24-token limit, request a long numbered list, select **Continue generating** and then **Stop generating**. Close the app, call `run_review` again, open the saved thread and delete the disposable conversation.
5. For an application approval check, disconnect the review machine from the network and run the same launch command without `unshare -Urn`. The namespace used above hides host-root ownership, so the app catalog fails closed there. With KCalc installed, request “Open KCalc”, inspect the application, desktop entry and executable, and exercise Deny, expiry and Approve once on separate requests.

Representative screenshots and the six-page PDF and lookalikes are under `target/task-05-evidence/`.
