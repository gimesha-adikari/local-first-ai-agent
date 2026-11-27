# Stage 03-B capability, data and session contracts

This record applies Blueprint v1.1 and the v0.3.0 plan at context revision
`c4f00047b5b0abceca9e5af582469b7af0ff0a80` to the accepted 03-A application
baseline `3d05cac2da351749def1a58c0b8f93acd1f3a1e4`.

## Request invocation

`agent-core::capability` describes an invocation by a versioned identifier and
associated input, output and delta types. Registration and discovery are
separate from policy admission. The local adapter reports its offers from the
loaded model, including whether its template supports a structured Agent
decision. Its offers specify on-device locality, text input and output,
streaming where applicable, token units, model weights and context requirements.
Language coverage for local generation is unspecified and depends on the model.
The existing BGE adapter continues to use the pinned English embedding model.
The Linux support check is owned by that adapter.

Chat and Agent use typed invocation through the same dedicated worker. The
loader still accepts selected local files, and the worker still serializes
generation, decisions and model lifecycle operations. llama.cpp owns template
rendering, token fitting, sampling and exact continuation replay. SQLite keeps
the existing continuation JSON format as an opaque checkpoint. The existing
model binding and adapter-side fingerprint and token validation still apply.

## Classification and persistence

The initial families are public, content, personal, other people, raw sensor
and secret. A trusted registry maps each versioned origin class to a family.
A transformation unions its input classes and can add a derived class.
Sharing consumes authorization for the specific operation and leaves the
classes intact. The webview and model have no label-setting interface.

User text, saved memories, library passages and Agent observations are
classified at their Rust ingress or storage boundary. Library passage
provenance comes from its saved page or line coordinates. Vectors inherit
their source classes. Persistent library vectors reconstruct classification
from the existing document revision and chunk relationship. A Chat result
inherits its fitted prompt, system prompt, history, source and memory classes.
Agent results inherit the fitted history and trusted observations consulted in
their task. Existing schema-11 records receive conservative personal classes on
read. Records with older incomplete source evidence receive the union of
plausible known source classes for that record type.

Schema 12 adds a nullable message classification and a cascading receipt table.
Legacy rows are left readable on upgrade. Chat commits fitted metadata before
streaming content and commits a terminal receipt in the transaction that
finishes the response. Each continuation has its own receipt. An interruption
during the process retains the existing interrupted-message recovery. A crash
before terminal commit can therefore leave checkpointed labels and an
interrupted response without a terminal receipt. Agent answer labels are
committed with task finalization. The existing Agent journal records its
lifecycle and actions.

## Intelligence authorization

The Rust policy checks observation, processing, retention and sharing
independently. The existing `PermissionEngine` remains authoritative for
closed Agent action kinds and their exact operations. Built-in on-device
workflow rules preserve existing local behavior. Native selected-document
import uses separate expiring authorizations to read, extract and retain the
pinned selection. The kernel rejects unknown identifiers, ungoverned
connectivity, secret-class content and a provider whose locality differs from
the requested locality.

Explicit grants are in memory and scoped to the immutable operation and
session. That operation binds the purpose, classes, intended and actual
locality, destination and retention. The kernel validates expiry against a
monotonic clock. Revocation, closure and cancellation invalidate subsequent
admission. There is no permission onboarding or persistent remote grant in
03-B.

## Egress contract and the 03-C transport requirements

The engine owns the transport. A provider adapter receives neither a transport
handle nor a redeemable network permit. The boundary checks the exact
registered destination and the immutable labelled body, then requires both
processing and operation-specific sharing at the point of handoff. The
handoff and revocation are serialized. Sharing authorizes one handoff.
A redirect is returned as data and a new destination needs a freshly scoped
operation. The boundary checks cancellation and grant liveness again before
returning the response. A receipt reports the trusted transport's sent, unsent
or indeterminate status, even when a response is subsequently discarded.

03-C needs an implementation and verification of the following protections
before production traffic is enabled.

- Bind the authorized scheme, host, port, route, HTTP method, content and
  credential destination to the actual connection. Canonicalize before
  registration and validate certificate and hostname. Protect address selection
  and connection reuse against DNS rebinding, unintended private addresses,
  implicit proxies and endpoint aliasing.
- Enforce policy before connection and before sending headers or payload. Use
  bounded connection and transmission commitments. Disable automatic redirects,
  retries and provider SDK side requests. Treat any new hop or retry as a new
  authorization decision and keep one transport record per attempt.
- Abort queued and in-flight I/O on cancellation, expiry or revocation, and drop
  late responses. Test the race between revocation and the first write and
  distinguish bytes already transmitted, no transmission and uncertainty.
  The synchronous 03-B mock handoff does not establish socket-level behavior.
- Keep credentials at the trusted transport edge using native secure storage.
  Bound time, request size, response size and streaming buffers. Verify that
  logs, receipts, crash output and command surfaces exclude secrets and raw
  personal content. Verify that local-only operation opens zero connections.
- Resolve provider-side retention disclosures and real usage reporting before
  a user can authorize a provider. Maintain separate application and provider
  retention records. Reject integrations whose own network traffic cannot pass
  the boundary.

## Future full-duplex sessions

A future `open_session` would negotiate one typed capability and provider
offer, then return bounded typed input and output channels tied to a
cancellable session lease. Each frame or bounded group would carry its own
provenance. Observation, processing and egress would be revalidated where each
group crosses a boundary. Session expiry or revocation would stop intake,
discard queued data, abort provider I/O and close the provider session.
Receipts would summarize the effective inputs, gaps, actual transmissions,
retention, and terminal reason from system records. The local store remains
authoritative for continuity. Stage 03-B uses the existing operation gate and
implements only request invocations.

## Verification

The locked, offline packaged workspace regression passed 204 tests, with
42 opt-in tests skipped by the default run. This includes the seeded SQLite
migration cases, fitted mixed-class Chat context, terminal cancellation and
the mock transport contract tests. All-target release Clippy passed with
warnings treated as errors. Frontend tests passed 61 of 61 and the production
frontend built with the existing chunk-size warning.

The local LFM replay check reproduced exact continuation across two segments
and retained the partial response when cancelled. It exercised the existing
tokenizer and checkpoint format using the installed GGUF.

Schema 12 reads existing profiles. The pre-upgrade application still rejects
a newer schema version, so an in-place binary downgrade requires the
pre-upgrade database.

The packaged desktop fixtures passed for unified Chat and Agent, contextual
Chat, semantic retrieval, the larger knowledge and memory integration case,
and native knowledge import. The import check exercised the picker,
cancellation, retention refusal, TXT import, duplicate handling, Markdown
replacement, PDF import, search and deletion. The contextual and larger
integration fixtures used the installed LFM and BGE files and isolated
profiles. Their saved Chat receipts recorded completed, cancelled or failed
invocations and contained zero external transmission records. A separate
real Qwen3.5 Agent test passed the large native-observation context fit.

The Linux release executable built with the packaged protocol, opened a
visible window under isolated X11 and created its temporary SQLite profile.
