# SPDX-License-Identifier: AGPL-3.0-or-later
"""One-shot SearXNG search worker for the Stage 04-C2 feasibility probe.

It imports an unmodified, pinned SearXNG source tree and replaces one method,
``Network.call_client``, so that every upstream request is described to the
host over a private pipe instead of being sent. The worker never creates a
curl client and runs in a sandbox that denies sockets. It answers exactly one
search and exits: no listener, no daemon, no persistent cache.

Protocol: one JSON object per line.
  worker -> host  ready | request | results | fatal
  host -> worker  search | response | refused

This file is combined with SearXNG at run time, so it is licensed like
SearXNG and is kept apart from the Apache-2.0 application.
"""

import asyncio
import base64
import concurrent.futures
import json
import logging
import os
import resource
import sys
import threading
import traceback
from timeit import default_timer

PACK = os.path.dirname(os.path.abspath(__file__))
sys.path[:0] = [os.path.join(PACK, "searxng"), os.path.join(PACK, "site-packages")]

# Keep the protocol channel private: anything a library prints goes to stderr.
CHANNEL = os.fdopen(os.dup(1), "wb", buffering=0)
os.dup2(2, 1)
logging.disable(logging.CRITICAL)


class Bridge:
    """Request/response pairs multiplexed over the pipe."""

    def __init__(self):
        self.lock = threading.Lock()
        self.pending = {}
        self.sequence = 0
        self.job = concurrent.futures.Future()

    def send(self, message):
        data = json.dumps(message, ensure_ascii=False).encode() + b"\n"
        with self.lock:
            view = memoryview(data)
            while view:
                view = view[CHANNEL.write(view) :]

    def request(self, method, url):
        future = concurrent.futures.Future()
        with self.lock:
            self.sequence += 1
            number = self.sequence
            self.pending[number] = future
        self.send({"type": "request", "id": number, "method": method, "url": url})
        return future

    def read(self):
        for line in sys.stdin.buffer:
            reply = json.loads(line)
            if reply["type"] == "search":
                self.job.set_result(reply)
                continue
            future = self.pending.pop(reply.get("id"), None)
            if future is not None:
                future.set_result(reply)
        # The host closed the channel: stop at once.
        os._exit(3)


BRIDGE = Bridge()


def main():
    # Running from the start, so a request made during initialization is
    # answered (the host refuses it) instead of blocking.
    threading.Thread(target=BRIDGE.read, daemon=True).start()

    from curl_cffi.requests import Headers
    from curl_cffi.requests.exceptions import RequestException
    from curl_cffi.requests.models import Request

    from searx import settings
    from searx.engines import load_engines
    from searx.extended_types import SXNG_Response
    from searx.metrics import initialize as initialize_metrics
    from searx.network import initialize as initialize_network
    from searx.network.network import Network

    async def call_client(self, stream, method, url, **kwargs):
        raise_on_error = Network.extract_do_raise_for_httperror(kwargs)
        Network.extract_kwargs_clients(kwargs)
        if stream or any(kwargs.get(key) for key in ("data", "json", "content", "params", "auth")):
            raise RequestException("the host bridge carries plain requests only")
        # Headers and cookies are dropped here: the host sends its own.
        reply = await asyncio.wrap_future(BRIDGE.request(method.upper(), url))
        if reply["type"] != "response":
            raise RequestException(f"refused by the host: {reply.get('reason')}")
        response = SXNG_Response()
        response.url = url
        response.status_code = reply["status"]
        response.reason = ""
        response.ok = reply["status"] < 400
        response.content = base64.b64decode(reply["body"])
        response.headers = Headers({"content-type": reply.get("contentType") or ""})
        response.request = Request(url, Headers(), method.upper())
        return self.patch_response(response, raise_on_error)

    async def no_client(*_args, **_kwargs):
        raise RequestException("direct networking is disabled")

    Network.call_client = call_client
    Network.get_client = no_client

    # The subset of searx.search.initialize this worker needs. Plugins,
    # answerers, autocomplete, favicons and the web application stay unloaded.
    engine_settings = settings["engines"]
    load_engines(engine_settings)
    initialize_network(engine_settings, settings["outgoing"])
    initialize_metrics([engine["name"] for engine in engine_settings], False)

    from searx.results import ResultContainer
    from searx.search.models import EngineRef, SearchQuery
    from searx.search.processors import PROCESSORS

    PROCESSORS.init(engine_settings)
    BRIDGE.send({"type": "ready", "engines": sorted(PROCESSORS)})
    job = BRIDGE.job.result()

    names = [name for name in job["engines"] if name in PROCESSORS]
    query = job["query"]
    search = SearchQuery(query, [EngineRef(name, "general") for name in names], lang="en-US", safesearch=1)
    container = ResultContainer()
    start = default_timer()
    limit = float(job.get("timeout", 12))
    threads = []
    for name in names:
        processor = PROCESSORS[name]
        params = processor.get_params(search, "general")
        if params is None:
            continue
        thread = threading.Thread(
            target=processor.search, args=(query, params, container, start, limit), daemon=True
        )
        thread.start()
        threads.append(thread)
    for thread in threads:
        thread.join(max(0.0, limit - (default_timer() - start)))
    container.close()

    results = []
    for result in container.get_ordered_results()[:40]:
        published = getattr(result, "publishedDate", None)
        results.append(
            {
                "url": str(result["url"] or ""),
                "title": str(result["title"] or "")[:600],
                "content": str(result["content"] or "")[:2000],
                "engine": str(result["engine"] or ""),
                "publishedDate": published.isoformat() if hasattr(published, "isoformat") else None,
            }
        )
    BRIDGE.send(
        {
            "type": "results",
            "results": results,
            "unresponsive": sorted(
                [item.engine, str(item.error_type)] for item in container.unresponsive_engines
            ),
            "maxRssKib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
        }
    )
    CHANNEL.close()
    os._exit(0)


if __name__ == "__main__":
    try:
        main()
    except BaseException as error:  # noqa: BLE001 - report and exit
        traceback.print_exc()
        BRIDGE.send({"type": "fatal", "error": type(error).__name__, "detail": str(error)[:300]})
        os._exit(1)
