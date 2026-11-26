# Stage 03-C cloud providers, credentials and model freedom

This record applies Blueprint v1.2 and the v0.3.0 plan at context revision
`b08fd9fe3f940fb23f6207d362bb967ec90f33e3` to the accepted 03-B application
baseline `f917681433daf20b85cd4aa1806d6dc2223304e8`. It resolves OQ-1 and
OQ-11 as the owner decided for this stage.

## Decisions applied

- **OQ-1.** The Debian base package carries no language or embedding model
  weights. Existing profiles, model rows and settings are kept. Local models
  are optional imports, verified against a curated, hash-pinned manifest.
  There is no downloader.
- **OQ-11.** INV-10's "IPC" means application IPC: Tauri commands and the
  webview. Credentials are entered in a native GTK dialog owned by Rust and
  stored through the Secret Service over an encrypted (Diffie-Hellman) D-Bus
  session. They exist transiently in Rust memory (zeroized wrappers) and in
  the request written by the trusted transport. There is no plaintext,
  file, environment or session-only fallback; unavailable storage fails
  closed.

## Network boundary

`engine::network` owns the only HTTP client. Adapters build an immutable
`Outbound` (method, fixed route, labelled body) and parse response bytes;
they get no socket, credential or permit. The transport module is private
to the boundary.

| 03-B requirement | Implementation | Verified by |
|---|---|---|
| Canonical destinations | `Endpoint::parse` normalizes scheme, host (punycode, IPv4 forms, mapped IPv6, trailing dot), port and path; rejects user info, query, fragment, encoded or dot paths. `Destination` binds identifier, endpoint, locality, network scope and credential binding; any change invalidates bound operations. | `endpoint::tests`, `policy_tests::denies_unknown_ids_endpoint_changes_routes_and_unverifiable_adapters` |
| Bind scheme, host, port, route, method, payload, credential destination | `Operation` carries the destination and a `RequestBinding` (method, route, SHA-256 and length of the body). Grants are per operation instance. The credential key is derived from the destination's own origin and rechecked before reading. | `policy_tests::sharing_is_one_operation_bound_to_its_payload`, `transport_tests::delivers_the_bound_request_with_its_credential_and_records_it` |
| TLS and host name | rustls 0.23 with ring, system trust anchors, ALPN `http/1.1`, no resumption or early data. | `transport_tests::tls_requires_a_trusted_certificate_for_the_exact_host` (untrusted issuer, wrong host, expired) |
| DNS rebinding, aliasing, private networks | One resolution per request; every answer must fit the scope (`Public`, `Loopback`, or `Pinned` local IP literals); mixed answers are refused; the connected peer is rechecked. Cloud providers need HTTPS on public addresses. | `address::tests`, `transport_tests::resolved_addresses_must_fit_the_destination_scope` |
| Explicit self-hosted endpoints | Loopback by name or address, or a pinned local-network IP. A key is never configured for unencrypted HTTP beyond loopback. | `endpoint::tests::scope_rules_bind_locality_scheme_and_address`, `remote_tests::keys_are_never_configured_for_unencrypted_network_servers` |
| No proxies, redirects, retries or SDK traffic | Direct connection to the validated address; proxy variables are never read; 3xx is returned as data; one attempt; no SDK. | `transport_tests::redirects_are_returned_as_data_and_never_followed`, `transport_tests::failures_are_not_retried_and_proxy_settings_are_ignored` |
| Policy before connection and before headers | `admit` before queueing, resolution, connection and TLS; `commit` consumes the sharing grant under the revocation lock immediately before the first request byte; liveness before every write slice and response piece. | `transport_tests::revocation_ordered_after_commit_but_before_the_first_write_sends_nothing` |
| Cancellation, expiry, revocation while queued and in flight | Queued work waits on a bounded slot pool while rechecking admission. Uploads check before each write slice and recheck on 10 ms ticks while a write is pending. Local-only mode and provider removal also invalidate. | `transport_tests::revocation_while_queued_drops_the_request_without_a_connection`, `cancellation_expiry_and_revocation_interrupt_a_stalled_request_body`, `cancellation_during_streaming_stops_delivery`, `expiry_cancellation_and_missing_credentials_stop_before_connecting` |
| Bounded sizes and time | Request body 6 MiB; head 64 KiB and 64 headers; body 16 MiB; SSE line 1 MiB; 16 KiB pieces with an 8-piece queue; timeouts for resolution, connection, TLS, credential, head, idle and total. | `transport_tests::response_and_header_sizes_and_idle_time_are_bounded`, `http::tests`, `openai::tests::rejects_malformed_and_oversized_streams` |
| Late responses discarded | A response arriving after revocation or expiry is dropped. | `transport_tests::revocation_or_expiry_after_sending_discards_the_late_response` |
| Truthful transmission records | Bytes are counted at the TCP layer after the handshake: `unsent` (none), `sent` (the whole request was written and flushed), `unknown` (partial or failed write). Each attempt has one record with origin, method, route, body digest, credential flag, labels and grants. | All transport tests assert the record |
| Local-only opens no connections | Policy denies before resolution; the transport thread is created only on the first admitted request. | `transport_tests::local_only_and_unregistered_destinations_open_no_connection`, `remote_tests::local_only_mode_opens_no_connection`, providers WebView fixture |

The Secret Service client is accepted only on a local (`unix:`) session bus,
so it cannot become an ungoverned network connection.

## Provider adapter

`providers::openai` targets `/chat/completions` and `/models`. It supports a
configurable endpoint and model, text Chat, typed image parts (data URLs)
when the provider is configured for images, streaming SSE or a whole JSON
response, cancellation, optional `stream_options.include_usage`, and
normalized errors (authentication, rate limit or quota, context overflow,
content filter, unavailable, rejected, invalid response, network, TLS,
policy, credential and budget). Provider messages are stripped of control
characters, redacted of key-like tokens and bounded. Offers advertise
locality, text or text-and-image input, streaming, the configured context,
usage units, configured prices and the declared retention. Nothing else is
assumed about a compatible server.

Remote Chat uses the same preparation as local Chat. Before building the
request it withholds what the provider may not receive:

- the current message must be shareable, or the turn stops;
- document evidence requires document sharing, or the turn stops with an
  explanation instead of a silent change of answer;
- memories without memory sharing are withheld;
- history is kept only as a contiguous newest suffix whose labels are all
  allowed.

The remainder is fitted to the configured context with a conservative
estimate (one token per two bytes), shedding memories, then evidence, then
the oldest history. A consent check refuses any payload label outside the
provider's allowed classes, and refuses providers with unknown retention.
Receipts record used and withheld sources, the egress record, provider
retention, usage and its basis. Thinking and Continue stay local-only;
Agent mode always uses the local model. There is no automatic fallback in
either direction.

## Credentials

- Entry: `provider_enter_credential` opens a modal GTK dialog with a masked
  entry. The value goes from GTK into zeroizing memory and to the Secret
  Service. The command returns only `stored` or `cancelled`.
- Storage: items carry only application, kind, provider key and canonical
  origin attributes. Store, status, read and delete are each bounded
  (3 s, 10 s for store) and map unavailable, locked or failing storage to
  errors. Changing a provider's origin or deleting it deletes the key.
- No command accepts or returns a credential; `ProviderSettings` rejects
  unknown fields, so a key cannot be smuggled into configuration.

## Usage and budgets

A reservation of the estimated input plus the full output limit is written
in the same immediate SQLite transaction that checks the month's reserved
and charged totals against the token and spending limits. After the
exchange it is settled from the transport record and the provider's
report: reported usage when present; nothing when unsent; otherwise the
full reservation as an estimate. Interrupted reservations are charged in
full at startup. A spending limit needs prices; unpriced earlier usage is
priced at the highest configured rate. Limits are enforced against
application estimates before sending. They are not a guarantee of the
provider's bill, and the interface says so.

## Packaging and model import

`tauri.bundled-model.conf.json` and the staging script are removed;
`tauri.deb.conf.json` builds the base package without model resources and
recommends a keyring service. Schema 13 adds providers, intelligence
settings (existing profiles stay local-only), the usage ledger and a model
`artifact_key`. The manifest (`engine::manifest`, version 1) pins LFM2.5
1.2B Instruct Q4_K_M and BGE small English v1.5 F16 by size, SHA-256,
source revision, license and capabilities. An import is hashed only when its
size matches an entry; a match is shown as Verified, anything else as
Unverified. Without an embedding model, existing keyword retrieval applies
and the coverage records the requested mode.

For an in-place Debian upgrade, the pre-install hook verifies the legacy
LFM2.5 file and retains it under `/var/lib/local-first-ai-agent/models/`
before dpkg removes the old package resource. It hard-links the existing
file on the same filesystem and copies it with its existing permissions when
the filesystems differ. If preservation or verification fails, unpacking
stops before dpkg removes the old file. Install the new package over the old
one in the same transaction so the hook can access the artifact. A fresh
installation on a system without retained state has no models.

The application verifies the retained file and updates the existing model
row by its bundled identity. The model ID, settings and last-loaded choice
carry forward. The hook operates on the single known package path. To
exercise the upgrade in disposable dpkg roots, run
`sh scripts/check-deb-model-upgrade.sh OLD_BUNDLED.deb NEW_BASE.deb`.

## Interface

Settings gains a Providers category: local-only mode, the Chat target,
provider cards (readiness, address and encryption, shared and withheld
categories, retention, capabilities, measured, estimated and reserved usage,
estimated cost and limits) with key entry, removal, connection test, edit
and delete, and a form for cloud and self-hosted providers. Chat shows the
provider in the top bar and a disclosure above the composer before sending.
A fresh profile without models offers local, cloud and self-hosted setup.
Models shows Verified and Unverified import badges. No credential value is
ever in frontend state.

## Verification

All results are from this machine (i5-8500, Linux, GNOME keyring 50).

- Locked workspace tests with the packaged protocol: 252 passed, 0 failed,
  46 ignored (the 42 existing opt-in real-model and desktop tests plus four
  new isolated-environment tests listed below). New suites: endpoint, address, HTTP framing, 13 socket and TLS
  transport tests against loopback fixtures, 6 policy tests, provider
  adapter tests, 9 remote Chat engine tests, storage provider and upgrade
  tests, manifest and base-package tests.
- Release Clippy with warnings as errors: clean. `cargo fmt --check`: clean.
- Frontend: 65 tests, type-check and production build (existing chunk-size
  warning).
- Real WebView, isolated X11 and profile, network namespace with loopback
  only: `providers_ipc_desktop_fixture` (local-only refusal, discovery,
  cloud-only Chat without a model, usage reconciliation, credential fields
  rejected) and `credential_entry_desktop_fixture` (native GTK dialog typed
  through XTest, key stored in a temporary keyring, attached only at the
  transport edge, removal fails closed; the key was absent from every
  profile file and the database). `unified_ipc_desktop_fixture` passed with
  networking fully disabled.
- Secret Service in isolated D-Bus sessions: round trip with origin binding
  in a temporary unlocked keyring, and fail-closed behavior with no service.

**Simulated versus actual.** Every remote exchange used synthetic data
against loopback HTTP or TLS fixtures with fixture certificates. No paid
or third-party provider was contacted and no public-internet TLS or DNS
path was exercised.

### Final acceptance preparation

- The Linux release build, scoped release Clippy, and formatting checks
  passed. The packaged executable opened in an isolated X11 session.
  The frontend build reported its existing bundle-size warning.
- The Debian fixture upgraded the bundled 0.1.0 package to the model-free
  0.2.0 package with the same GGUF inode, digest and mode. It also passed the
  fresh-install, corrupt-file refusal, and repeat-unpack cases.
- Five bundled-model tests passed. The upgrade test uses a temporary
  database to check the model ID, custom settings, restore choice and an
  unrelated import. Fourteen loopback transport tests passed. The new upload
  regression stalls after the first body byte and checks that cancellation,
  grant expiry and revocation each leave the body incomplete and record the
  transmission as `unknown`.
- The four new opt-in tests passed in separate isolated runs.
  `providers_ipc_desktop_fixture` used the loopback provider.
  `credential_entry_desktop_fixture` used X11, a synthetic key and a temporary
  keyring. `secret_service_round_trip_in_isolated_session` used its own
  unlocked keyring. `missing_secret_service_fails_closed` used a separate
  D-Bus session with no Secret Service.

## Manual cloud acceptance

1. From the repository root, launch `./target/release/local-first-ai-agent`
   in the regular Linux desktop session, or open the installed application.
2. Open **Settings → Providers → Cloud provider**. Enter the provider's
   documented HTTPS API base URL and exact Chat Completions model ID.
   Set the model's context window and a small **Output limit**. Declare
   the provider's actual retention policy and enter its input and output
   prices per million tokens to see estimated cost. Keep **Conversation
   history**, **Saved memories** and **Document excerpts** off for this check.
   Save the provider.
3. On the provider card, click **Enter API key…**. Type the credential into
   the native GTK dialog and click **Save in keyring**.
4. Turn **Local-only mode** off. Under **Chat answers with**, select the
   provider. Open a new Chat with no attachments. Confirm the provider and
   sharing disclosure above the composer, then send
   `Reply with exactly: Stage 03-C check.`
5. Return to the provider card and inspect **Usage**. Compare the reported
   or estimated tokens and estimated cost with the provider's dashboard.
   The **Details** drawer shows response stats. The full receipt is stored
   in the local SQLite database. Inspect the latest cloud receipt with this
   read-only command, then check its provider, retention, usage basis,
   egress labels and `transmission: "sent"`.

   ```sh
   python3 - <<'PY'
   import json
   import os
   from pathlib import Path
   import sqlite3

   data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
   db = data / "io.github.gimesha-adikari.localfirstaiagent/chat/conversations.sqlite3"
   with sqlite3.connect(db.resolve().as_uri() + "?mode=ro", uri=True) as connection:
       row = connection.execute("""
           SELECT message_id, receipt FROM intelligence_receipts
           WHERE json_extract(receipt, '$.provider.locality') = 'cloud'
           ORDER BY id DESC LIMIT 1
       """).fetchone()
   if row is None:
       raise SystemExit("No cloud Chat receipt found.")
   print(json.dumps({"messageId": row[0], "receipt": json.loads(row[1])}, indent=2))
   PY
   ```

6. Turn **Local-only mode** back on in **Settings → Providers**. Confirm
   **Chat answers with** shows **Local model** and the provider card shows
   **Off · local-only mode**.

## Limitations

- Self-hosted HTTPS needs a certificate trusted by the system store; there
  is no per-provider certificate pinning.
- The token estimate is coarse and ignores provider tokenizers; providers
  that omit usage are charged at the full reservation.
- GTK and the Secret Service client may hold copies of the key outside the
  zeroizing wrappers until their memory is reused.
- A malicious provider that already holds a key could echo it in streamed
  content; only error messages are redacted.
- Reasoning fields from some servers are ignored.
- The upgrade hook needs the previous on-disk package resource. If it was
  already removed, recovery requires the original Debian package or a
  verified local copy.
- KDE, X11-only desktops and a locked keyring prompt were not tested.
