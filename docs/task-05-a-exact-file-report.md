# Task 05-A exact-file Agent report

**v0.1.0 Linux amd64 · 26 November 2026**

## Root cause and correction

The original Qwen decision grammar accepted a generic JSON argument object. The prompt also showed a folder-read example when only an exact file was selected. Qwen included `relative_path` with a file grant and the permission engine denied the call before dispatch.

The Qwen prompt schema and decoding grammar now bind each visible active ID to its grant kind. An exact-file read has only `scope_id` and optional `pdf_pages`. A directory read also requires a relative path, which the registry still validates. Invalid arguments stay subject to typed validation. A file-read shape denial can request a fresh proposal within the existing retry budget, with a fixed correction that omits the rejected arguments. The runtime authorizes any new proposal against live grants. The rejected operation receives no permit.

## Security and engineering verification

| Check | Result |
| --- | --- |
| Scope contracts | Model-free tests covered exact, directory and coexisting grants, and excluded revoked and expired IDs from the Qwen grammar. |
| Native reading | Exact-file TXT and directory TXT returned the expected distinct contents with both grants active. Native PDF tests covered exact-file reads, explicit page ranges, malformed and unsupported inputs, and parser cancellation. |
| Rejected arguments and leases | Registry, permission and native tests passed for invalid paths and ranges, missing, revoked and expired scopes, and traversal and link rejection. Scripted Agent integration verified one corrected dispatch and zero dispatches after two unsuccessful corrections. |
| Engineering | Formatting, locked release workspace Clippy with `-D warnings`, and Git diff checks passed. Locked release workspace tests passed with 123 passed and 28 opt-in tests skipped in the ordinary run. Three opt-in isolated native PDF tests passed separately. TypeScript and the Vite production build passed during bundling. |

## Real-model desktop verification

The rebuilt package ran with the existing Qwen3.5-4B GGUF in a private X11 session and an offline network namespace. A disposable profile contained the five preceding Task 05 turns. The original request was submitted once with only the exact PDF selected through the native picker.

- The native result extracted **page 5 of 6** and reported that the other five pages were unread in that step. The page text says the spare pump is in the shed.
- Qwen answered “Page 5 states that there is a spare pump in the shed.” The turn completed in about 138 seconds at the existing 300-second limit, with one successful `files.read` and zero retries.
- The scoped execution record and saved answer matched the UI. SQLite integrity and foreign-key checks passed. [Desktop evidence](../target/task-05a-evidence/exact-file-desktop.png)

## Package

`target/release/bundle/deb/local-first-ai-agent_0.1.0_amd64.deb` · **723,573,336 bytes (690.05 MiB)**

SHA-256 `668c3f3d9953df5482e748425c8562021debdf21042c0d28440bb86238016675`

Built once after the correction with the staged model and existing caches. The packaged GGUF checksum, model license and notice, and the runtime dependencies checked out. The earlier package was retained separately before replacing the standard build output.

## Remaining limitations

The real-model pass covers one exact-file PDF request. Exact-file TXT, directory reads, coexisting grants and rejection behavior were verified with deterministic and native tests. The broader Task 05 desktop suite was outside this run. Release approval remains pending manual review.
