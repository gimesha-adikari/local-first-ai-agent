# Task 07-A distribution notices

**v0.1.0 · Linux amd64 · 26 November 2026**

## Licensing completed

- The approved **Apache-2.0** license now covers the original application source. The repository includes the official Apache license text. Cargo workspace members, the npm manifest and lockfile root, and Tauri bundle metadata declare Apache-2.0. Dependency versions are unchanged. [Official Apache-2.0 text](https://www.apache.org/licenses/LICENSE-2.0.txt)
- Debian installs `copyright`, `LICENSE`, `NOTICE` and the Rust standard-library copyright report under `/usr/share/doc/local-first-ai-agent/`. The component index covers 172 Cargo packages from the normal Linux application graph, 106 npm production packages, and three frontend build packages that contribute to the output. It records source versions and reproduces upstream legal texts, including Unicode-3.0 data terms, Apache terms and attributable notices, and vendored llama.cpp and SQLite materials. System shared libraries and PDF utilities are identified separately. [Software copyright index](../third_party/software-notices/copyright) · [Debian copyright guidance](https://www.debian.org/doc/debian-policy/ch-docs.html#copyright-information)
- The package supplies the exact unmodified MPL-2.0 `option-ext 0.2.0` source archive under `/usr/share/doc/local-first-ai-agent/sources/`. Its SHA-256 matches Cargo.lock. The copyright file explains how to obtain it and includes its license text. [MPL-2.0 §3](https://www.mozilla.org/en-US/MPL/2.0/)
- The bundled GGUF, LFM Open License v1.0 and model notice remain byte-for-byte unchanged. The model's commercial-use terms are separate from the application license. [Bundled model notice](../third_party/model-licenses/LFM2.5-1.2B-Instruct/NOTICE.md)

## Replacement package

`target/release/bundle/deb/local-first-ai-agent_0.1.0_amd64.deb` · **723,691,384 bytes (690.17 MiB)**

SHA-256 `ff0daea7989611feba6770b8e0dfc0c914ed0d2dcc42ebb6afb0a29a3b011c1a`

| Check | Result |
| --- | --- |
| Build and checks | One replacement build reused the cached dependencies and staged model. Formatting, diff checks, 123 locked Rust workspace tests, TypeScript and Vite passed. The prior package was retained separately. |
| Payload | All five software notice and source files matched their repository copies. The LFM files matched their previous SHA-256 or source copy. Runtime dependencies resolved in an APT simulation. |
| Offline startup | The extracted package launched in an offline network namespace. The bundled LFM2.5 model reached Ready, Settings displayed its model notice, and the disposable profile passed SQLite integrity and foreign-key checks. |

## Source and remaining blockers

The [source inventory](v0.1.0-source-inventory.txt) includes the new license, manifests, package configuration, software notice files, upstream MPL source archive and this report. Git's staging dry run left the index untouched. The [README](../README.md) has the replacement checksum and points to the installed notices.

The review covers the pinned Linux dependency graph and identified native and generated frontend materials. Exhaustive attribution of every object in the optimized executable remains unverified. The release revision still needs a green CI run, and approval is pending to commit and push. No commit, push, tag or publication was performed.
