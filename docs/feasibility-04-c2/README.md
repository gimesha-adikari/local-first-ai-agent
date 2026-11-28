# Stage 04-C2 feasibility: managed on-device web search

Date: 2026-11-27. Base: `4d9a62c`. Host: Linux x86_64, i5-8500, system Python
3.14.4, Bubblewrap 0.11.1.

## Verdict

**Do not ship a managed SearXNG provider.** The transport integration works and
is safe. The product goal, a free account-free *web* search, is not reachable
with it under this project's constraints, so no production provider, packaging
or UI was built. The application's behaviour is unchanged.

1. **No usable general web engine.** SearXNG's default general web results come
   from DuckDuckGo and an unofficial Google endpoint, which this stage excludes,
   as are the other large engines reviewed (table below). The keyless sources
   used here are specialised indexes and one small hobby-web index. Live,
   "history of the bicycle" returned ten results and, by title, none was about
   bicycles.
2. **Little left for SearXNG to do.** To authorize requests exactly, Rust must
   derive each upstream URL itself. For the four permitted engines SearXNG then
   contributes parsing and merging of three JSON APIs and one Atom feed, at the
   cost of a 137 MiB Python pack, interpreter-specific wheels, AGPL
   distribution duties and a second sandbox.

None of the stop conditions about forking or policy relaxation was met. The
blockers are engine access terms and the distribution footprint for the value
delivered.

## What was built

All of it is experimental and absent from application builds.

| Part | Location |
|---|---|
| Rust broker and tests (`#[cfg(test)]`) | `crates/engine/src/network/managed_probe.rs` |
| Test-only query string on a registered route (17 lines) | `network/{mod,transport,page,artifact}.rs` |
| SearXNG bridge worker (AGPL-3.0-or-later) | `probe/worker.py` |
| Hostile stand-in worker | `probe/hostile_worker.py` |
| Pack build, settings, hash-locked requirements | `probe/build_pack.sh`, `settings.yml`, `requirements.lock` |

SearXNG is pinned to `b5605b8f2fa2591108ab8e3f4fc16e88f7d66fe4` (2026-11-26) and
its source is unmodified. The worker replaces one method,
`searx.network.network.Network.call_client`, and disables `get_client`. It
drives the engine processors and `ResultContainer` directly: no Flask server, no
listener, no plugins. It runs one search over its stdin/stdout pipes and exits.
The build writes `searx/version_frozen.py`, upstream's own mechanism for running
without Git.

**Authorization.** The worker only proposes a URL. The broker accepts a `GET`
whose scheme, host, path and decoded parameters equal what Rust derives from the
authorized query for an authorized engine, once per engine, within the consent's
request budget. It then sends its own encoding with the boundary's fixed
headers; the worker's bytes, headers and cookies never leave. Each exchange has
its own one-handoff `Process` and `Share` grants as `web.search` to a registered
`search/managed/<engine>` destination. The policy kernel is unchanged.

**Containment.** All namespaces unshared (no usable network), seccomp denying
sockets, `io_uring`, new processes and `ptrace`, read-only system libraries and
pack only, 16 MiB private `/tmp`, no `/home`, `/etc` or `/proc`, cleared
environment, `prlimit` bounds on memory, CPU, file size and descriptors, a wall
deadline, 1 MiB per message and 64 messages.

## Verification

```sh
sh docs/feasibility-04-c2/probe/build_pack.sh /tmp/search-pack
AGENT_MANAGED_SEARCH_PACK=/tmp/search-pack cargo test --release -p engine --locked \
  --lib managed_probe -- --include-ignored --skip live_managed_search --test-threads=1
AGENT_LIVE_WEB=1 AGENT_MANAGED_SEARCH_PACK=/tmp/search-pack \
  cargo test --release -p engine --locked --lib live_managed_search -- --ignored --nocapture
```

Offline, against loopback TLS fixtures (6 tests passed):

- Socket creation (Python and libc), raw sockets, DNS, `io_uring`, `fork`,
  subprocesses, `/etc/passwd`, `/home`, `/proc` and writes all failed; threads
  worked.
- A request made during startup was refused. Real SearXNG made none.
- Refused with nothing sent: POST, another query, an extra parameter, `http`,
  other, look-alike and loopback hosts, URL credentials, another port or path,
  an unauthorized engine and a repeated request. One request reached the
  fixture, without cookie, credential or referrer.
- Resolver answers of private, loopback, link-local and `::1` addresses opened
  no connection.
- Local-only mode and cancellation during an in-flight request stopped the
  search in about 9 ms, left no process and voided the authorization.
- Memory, output flood, message flood, CPU spin and early exit were bounded.
- Real SearXNG returned normalized results through the existing SearXNG
  normalizer, with one `Sent` receipt per engine. Receipts hold the fixed route,
  never the query.

Not covered: the Chat research turn, page retrieval after a managed search,
consent UI, aarch64 and a cold-cache start.

## Live results

Three synthetic queries, one request per engine each, four seconds apart. All
twelve requests were sent and every engine answered.

| Query | Time | Peak RSS | Top ten after merging |
|---|---|---|---|
| rust borrow checker | 6.8 s | 63 MiB | 9 Stack Overflow, 1 small-web page; mostly on topic |
| sqlite write ahead logging | 4.7 s | 63 MiB | mixed, about half on topic, including `sqlite.org/wal.html` |
| history of the bicycle | 5.4 s | 63 MiB | none on topic by title |

Worker ready in 0.96–1.03 s; the rest is upstream latency. One run is not a
reliability measurement. The pinned Stack Exchange engine sorts by activity, not
relevance, which accounts for much of the noise.

## Engines

Checked 2026-11-27. This records what was read, not legal advice.

| Engine | Finding | Probe |
|---|---|---|
| All (default) | curl_cffi imitates Chrome's TLS and headers; the boundary does not | - |
| DuckDuckGo | engine imitates Firefox and browser navigation headers; further pages need a `vqd` bot-validation token | excluded |
| Google CSE | unofficial endpoint with a token taken from a third party's search-engine ID; Google's terms restrict automated access and scraping | excluded |
| Bing, Brave, Startpage, Mojeek | `robots.txt` disallows the search path; Mojeek's terms bar automation without API access; upstream marks Startpage and Mojeek inactive over proof-of-work CAPTCHAs | excluded |
| Wikipedia | Wikimedia requires contact details in the User-Agent; the engine refuses to load without `contact_url`; it looks up one title | excluded: owner decision |
| Mwmbl | developer page offers 1,000 anonymous requests a month, terms page requires a key; results CC BY-NC-SA 4.0 | excluded: unclear |
| Marginalia | the engine requires its own key | excluded |
| GitHub repositories | documented unauthenticated search, 10 requests a minute | included |
| arXiv | one request every 3 s, single connection; metadata CC0 | included |
| Wiby | invites applications to use its JSON output; small hobby web; no stated limits | included |
| Stack Overflow | about 300 anonymous requests a day per secondary sources; official pages could not be fetched | included, unverified |

Not every one of SearXNG's engines was reviewed.

## Distribution requirements (not built)

| Measure | Value |
|---|---|
| Installed pack (SearXNG `searx/` and wheels) | 137 MiB, about 4,450 files |
| Compressed | 31 MB zstd, 29 MB xz |
| Not included | CPython 3.14 and its standard library |
| Compiled modules | 12 tied to CPython 3.14, 1 stable ABI |

A shippable pack would need: a bundled interpreter or one pack per supported
system Python and architecture; a reproducible build published with a fixed
digest and verified provenance, stronger than the 04-B model checks; safe
extraction to an application-owned path; explicit installation consent; no
install scripts or runtime downloads. No artifact exists, so no download entry
was added.

## Licensing

- SearXNG is AGPL-3.0-or-later. `worker.py` imports it and carries the same
  licence ([text](probe/LICENSE-AGPL-3.0.txt)). Nothing of SearXNG is committed
  here or linked into the Apache-2.0 application.
- Distributing a pack requires the licence, notices and complete corresponding
  source for the exact revision, the worker and the build scripts.
- Open for legal review: whether an application speaking this pipe protocol to
  the worker is mere aggregation. Process separation alone does not settle it.
- Dependencies are mostly MIT and BSD, with certifi MPL-2.0. The curl_cffi wheel
  bundles native libraries whose notices would also have to ship, although this
  design never uses them to connect.

## Needed if this is revisited

- A general index whose terms permit automated, account-free use.
- Production transport support for a bound query string, with its digest in the
  receipt, and a policy rule specific to `search/managed/*`.
- Consent that names every engine and the number of requests.
- The distribution and licence items above.

If specialised keyless search is wanted instead, native Rust adapters for the
permitted APIs would reuse this authorization model without Python, AGPL
components or a subprocess. It must not be presented as general web search.
