#!/usr/bin/env python3
"""Delete station sessions older than KEEP_DAYS from OpenClaw (CT102, user openclaw).

The adapter opens a new core session per conversation (SESSION_IDLE_SECS), so
sessions pile up; only recent ones are worth keeping for debugging.

The timer is persistent: after a host reboot it fires as soon as the desktop
is up, often before the OpenClaw gateway answers (18.11.2026). So this script
waits for the gateway, keeps going when a single session cannot be deleted,
and fails only when the gateway never answered or the session list is broken.
"""
import json
import os
import socket
import subprocess
import sys
import time

KEEP_DAYS = float(os.environ.get("KEEP_DAYS", "7"))
PREFIX = "agent:main:station:"
DRY_RUN = os.environ.get("DRY_RUN") == "1"
READY_TIMEOUT = float(os.environ.get("READY_TIMEOUT", "180"))
GATEWAY = ("127.0.0.1", int(os.environ.get("GATEWAY_PORT", "18789")))


def wait_for_gateway(timeout=READY_TIMEOUT):
    deadline = time.monotonic() + timeout
    while True:
        try:
            with socket.create_connection(GATEWAY, timeout=2):
                return True
        except OSError:
            if time.monotonic() >= deadline:
                return False
            time.sleep(2)


def openclaw(*args):
    return subprocess.run(["openclaw", *args], check=True, capture_output=True, text=True).stdout


def main():
    if not wait_for_gateway():
        print(f"gateway {GATEWAY[0]}:{GATEWAY[1]} did not answer in {READY_TIMEOUT:g}s", file=sys.stderr)
        return 1
    try:
        sessions = json.loads(openclaw("sessions", "--json", "--limit", "all"))["sessions"]
    except (subprocess.CalledProcessError, ValueError) as error:
        detail = getattr(error, "stderr", "") or error
        print(f"cannot list OpenClaw sessions: {str(detail).strip()[:400]}", file=sys.stderr)
        return 1
    cutoff_ms = (time.time() - KEEP_DAYS * 86400) * 1000
    old = [s["key"] for s in sessions if s["key"].startswith(PREFIX) and s.get("updatedAt", 0) < cutoff_ms]
    failed = []
    for key in old:
        if DRY_RUN:
            print("would delete", key)
            continue
        try:
            openclaw("sessions", "delete", key, "--yes")
        except subprocess.CalledProcessError as error:
            # A session removed by another run must not stop the rest.
            failed.append(key)
            detail = (error.stderr or error.stdout or "").strip().splitlines()[-1:] or [str(error.returncode)]
            print(f"failed to delete {key}: {detail[0][:200]}", file=sys.stderr)
        except OSError as error:
            failed.append(key)
            print(f"failed to delete {key}: {error}", file=sys.stderr)
    verb = "found" if DRY_RUN else "deleted"
    print(f"station sessions: {len(old) - len(failed)} of {len(old)} older than {KEEP_DAYS:g} days {verb}"
          + (f", {len(failed)} failed" if failed else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
