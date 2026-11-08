#!/usr/bin/env python3
"""Delete station sessions older than KEEP_DAYS from OpenClaw (CT102, user openclaw).

The adapter opens a new core session per conversation (SESSION_IDLE_SECS), so
sessions pile up; only recent ones are worth keeping for debugging.
"""
import json
import os
import subprocess
import time

KEEP_DAYS = float(os.environ.get("KEEP_DAYS", "7"))
PREFIX = "agent:main:station:"
DRY_RUN = os.environ.get("DRY_RUN") == "1"


def openclaw(*args):
    return subprocess.run(["openclaw", *args], check=True, capture_output=True, text=True).stdout


def main():
    sessions = json.loads(openclaw("sessions", "--json", "--limit", "all"))["sessions"]
    cutoff_ms = (time.time() - KEEP_DAYS * 86400) * 1000
    old = [s["key"] for s in sessions if s["key"].startswith(PREFIX) and s.get("updatedAt", 0) < cutoff_ms]
    for key in old:
        if DRY_RUN:
            print("would delete", key)
            continue
        openclaw("sessions", "delete", key, "--yes")
    print(f"station sessions: {len(old)} older than {KEEP_DAYS:g} days {'found' if DRY_RUN else 'deleted'}")


if __name__ == "__main__":
    main()
