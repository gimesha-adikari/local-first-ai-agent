# Task 07 release preparation

**v0.1.0 · 26 November 2026**

Engineering is accepted. The Task 05-A candidate passed the exact-file
Qwen regression that was open in Task 05. Local release preparation is
complete, and publication is pending.
[Task 05 acceptance](task-05-integration-report.md) · [Task 05-A verification](task-05-a-exact-file-report.md)

## Candidate and CI

| Check | Result |
| --- | --- |
| Accepted Debian package | `target/release/bundle/deb/local-first-ai-agent_0.1.0_amd64.deb`, **723,573,336 bytes** |
| SHA-256 | `668c3f3d9953df5482e748425c8562021debdf21042c0d28440bb86238016675`, verified against Task 05-A |
| Package contents | The LFM GGUF matched its pinned checksum. Its packaged license and notice matched the source copies. The declared desktop and PDF dependencies resolved in a local APT simulation. |
| Local source checks | Rust formatting, Git diff whitespace, staging-script syntax and the proposed source inventory checked out. The accepted package was reused. |
| GitHub Actions | The model-free Ubuntu 24.04 workflow runs on push and pull request. It pins the actions and toolchains, tests frontend and Rust code, and builds the unbundled app. The latest `main` run at `18d9bb8` failed on Rust formatting. Clippy, Rust tests and the build were skipped. The current working tree passes formatting locally and still needs CI on the future release revision. [Failed GitHub job](https://github.com/gimesha-adikari/local-first-ai-agent/actions/runs/45419415097/job/138534505186) |

The package was accepted on Ubuntu 26.04.1. The executable references
glibc 2.39, and the CPU build uses AVX2, FMA, F16C and BMI2. The
model-free CI build does not qualify packaged runtime compatibility
on other hosts or run real-model acceptance.

## Documentation and source inventory

The [README](../README.md) now leads with model capabilities and the two
installation options. It covers exact-file and folder permissions, PDF
limits, CPU performance and local data handling. The
[release notes](release-notes-v0.1.0.md) describe the accepted scope and
the [roadmap](roadmap.md) records engineering acceptance with publication
pending.

The [proposed commit inventory](v0.1.0-source-inventory.txt) lists exact
paths, including both lockfiles, migrations, synthetic PDF test fixtures,
the packaging script and model license materials. It includes the removed
legacy Agent component so its deletion is captured. The Git dry run left
the index untouched. Model weights, build outputs, private profiles and
machine-specific paths are excluded. Existing desktop captures, archives and
historical working notes stay local.

## Licensing

- **Application**. The owner still needs to decide how to license the app.
  No application license was added.
- **Models**. The bundled LFM2.5 weights retain the LFM Open License v1.0.
  Include the license with redistribution. Section 5 limits commercial use
  by legal entities at the defined US$10 million annual revenue threshold,
  with a qualified non-profit provision for non-commercial or research
  use. The optional Qwen3.5-4B GGUF is labeled Apache-2.0 by its publisher
  and is imported separately. [Pinned LFM license](https://huggingface.co/LiquidAI/LFM2.5-1.2B-Instruct-GGUF/blob/8ed288026e23958ad9dfa92d53ed773a8eee7125/LICENSE) · [Verified Qwen GGUF](https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/blob/6727efff692673b7a2ebb4c6ba830ce4f5fb1309/Qwen3.5-4B-UD-Q4_K_XL.gguf)
- **Dependencies**. The local license inventory covers 319 third-party
  Cargo packages from the Linux resolution, including 172 in the normal
  application graph excluding procedural macros, and 220 npm packages
  split into 106 production and 114 development entries. The main
  families are MIT, Apache-2.0 and BSD. Review Unicode-3.0 ICU components,
  MPL-2.0 `option-ext`, and the MIT llama.cpp code vendored by its Rust
  bindings. SQLite's engine is public domain and its Rust wrappers are
  MIT. GTK, WebKitGTK, Poppler, Bubblewrap and util-linux are host
  dependencies. [Local license metadata inventory](../target/task-07-evidence/dependency-license-metadata.tsv) · [SQLite copyright](https://www.sqlite.org/copyright.html)
- **Distribution notices**. The candidate includes the model notices.
  The Debian payload has no consolidated software copyright and license
  file. Before distribution, assemble the applicable texts and copyright
  notices for the shipped code, including vendored native code and the
  embedded frontend. Include source-availability information for any
  distributed MPL-covered code and applicable Apache notices. Use
  `/usr/share/doc/local-first-ai-agent/copyright` for Debian copyright
  information. [MPL-2.0 §3](https://www.mozilla.org/en-US/MPL/2.0/) · [Apache-2.0 §4](https://www.apache.org/licenses/LICENSE-2.0) · [Debian copyright guidance](https://www.debian.org/doc/debian-policy/ch-docs.html#copyright-information)

## Proposed commit and release steps

1. Review the exact paths with
   `git add --dry-run --pathspec-from-file=docs/v0.1.0-source-inventory.txt`.
   After approval, stage those paths on a release branch and keep the
   manifests and lockfiles together. Review the staged diff for local data.
2. Resolve the application license and complete the software notices. Add
   the approved distribution files to the inventory. Build a replacement
   package if the notices are installed in it, verify its contents and
   checksum, and smoke-check the replacement.
3. Review and commit the release source, then obtain approval to push it
   for a green CI run. After approval to merge, tag and publish, attach
   the package, checksum and notices to the release. Keep weights and
   release artifacts out of the Git tree.

## Remaining blockers

The application license decision, complete distribution notices, green
CI for the release revision, and authorization to publish are still
pending. No package rebuild, commit, push, tag or release was performed
in Task 07.
