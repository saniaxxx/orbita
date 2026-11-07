#!/usr/bin/env python3
"""Deterministic Sber TV control over ADB.

This is an operational script, not an MCP server. It covers the TV that does
not expose Android TV Remote v2 (SaluteTV / TV-4JR): power, volume, keys and
VK search/play. The VK part drives the TV UI through `uiautomator`; the
keyboard layout is read from the UI dump, so it is not tied to fixed pixels.

Usage:
    ADB=/path/to/adb TV_ADB=192.168.250.111:5555 ./tv-adb.py state
    ... tv-adb.py power on|off
    ... tv-adb.py volume up|down|set N
    ... tv-adb.py key home|back|up|down|left|right|ok|play_pause|next|previous
    ... tv-adb.py vk-play "Шериф Лабрадор"

ADB is a full shell on the TV. Keep port 5555 on the trusted LAN only.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

ADB = os.environ.get("ADB", "adb")
SERIAL = os.environ.get("TV_ADB", "192.168.250.111:5555")
VK_PACKAGE = "com.vk.tv"
VK_ACTIVITY = f"{VK_PACKAGE}/.presentation.base.TvMainActivity"

KEYS = {
    "home": 3,
    "back": 4,
    "up": 19,
    "down": 20,
    "left": 21,
    "right": 22,
    "ok": 23,
    "play": 126,
    "pause": 127,
    "play_pause": 85,
    "next": 87,
    "previous": 88,
    "stop": 86,
    "mute": 164,
}


def run(args: list[str], check: bool = False) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(args, text=True, capture_output=True)
    if check and result.returncode != 0:
        raise RuntimeError(
            f"{' '.join(args)} failed ({result.returncode}): "
            f"{(result.stderr or result.stdout).strip()}"
        )
    return result


def adb(*args: str, check: bool = True) -> str:
    return run([ADB, "-s", SERIAL, *args], check=check).stdout


def connect() -> None:
    result = run([ADB, "connect", SERIAL])
    text = (result.stdout + result.stderr).strip()
    if "connected" not in text.lower() and "already" not in text.lower():
        raise RuntimeError(f"adb connect failed: {text}")
    adb("wait-for-device")


def dump_ui() -> ET.Element:
    adb("shell", "uiautomator", "dump", "/sdcard/window.xml")
    xml = adb("shell", "cat", "/sdcard/window.xml")
    return ET.fromstring(xml)


def center(node: ET.Element) -> tuple[int, int] | None:
    match = re.match(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", node.get("bounds", ""))
    if not match:
        return None
    left, top, right, bottom = map(int, match.groups())
    return (left + right) // 2, (top + bottom) // 2


def nodes(root: ET.Element):
    return root.iter("node")


def find_text(root: ET.Element, text: str) -> ET.Element | None:
    wanted = text.casefold()
    for node in nodes(root):
        if (node.get("text") or "").strip().casefold() == wanted:
            return node
    return None


def find_contains(root: ET.Element, text: str) -> ET.Element | None:
    wanted = text.casefold()
    for node in nodes(root):
        if wanted in (node.get("text") or "").casefold():
            return node
    return None


def tap(node: ET.Element) -> bool:
    point = center(node)
    if point is None:
        return False
    adb("shell", "input", "tap", str(point[0]), str(point[1]))
    return True


def tap_text(root: ET.Element, text: str) -> bool:
    node = find_text(root, text)
    return node is not None and tap(node)


def open_search() -> None:
    root = dump_ui()
    node = None
    for candidate in nodes(root):
        desc = (candidate.get("content-desc") or "").casefold()
        rid = candidate.get("resource-id") or ""
        if "введите запрос" in desc or rid.endswith("_SEARCH"):
            node = candidate
            break
    if node is None or not tap(node):
        adb("shell", "input", "tap", "72", "168")
    time.sleep(1.5)


def clear_search(root: ET.Element) -> None:
    if tap_text(root, "Очистить"):
        time.sleep(0.5)
        return
    # Fallback: move to the end and delete a generous number of characters.
    adb("shell", "input", "keyevent", "123")
    for _ in range(80):
        adb("shell", "input", "keyevent", "67")
    time.sleep(0.5)


def type_query(query: str) -> None:
    root = dump_ui()
    for char in query.upper():
        if char == " ":
            if not tap_text(root, "Пробел"):
                adb("shell", "input", "keyevent", "62")
            time.sleep(0.25)
            continue
        node = find_text(root, char)
        if node is None:
            node = find_text(root, char.lower())
        if node is None:
            raise RuntimeError(f"клавиша {char!r} не найдена на экранной клавиатуре")
        if not tap(node):
            raise RuntimeError(f"не удалось нажать клавишу {char!r}")
        time.sleep(0.25)
    tap_text(root, "Поиск")
    time.sleep(3.0)


def result_title(root: ET.Element, query: str) -> tuple[str, ET.Element] | None:
    tokens = [part for part in query.casefold().split() if len(part) > 2]
    for node in nodes(root):
        text = (node.get("text") or "").strip()
        folded = text.casefold()
        if len(text) < 8 or not any(token in folded for token in tokens):
            continue
        if folded in {"видео", "каналы", "плейлисты", "трансляции", "новые"}:
            continue
        point = center(node)
        if point is None:
            continue
        # Skip the search box and suggestion chips at the top; cards live in
        # the results grid lower on the screen.
        if point[1] < 500:
            continue
        return text, node
    return None


def vk_play(query: str) -> None:
    query = query.strip()
    if not query:
        raise RuntimeError("пустой поисковый запрос")
    adb("shell", "am", "force-stop", VK_PACKAGE)
    adb("shell", "am", "start", "-n", VK_ACTIVITY)
    time.sleep(3.0)
    open_search()
    root = dump_ui()
    clear_search(root)
    type_query(query)
    root = dump_ui()
    found = result_title(root, query)
    if found is None:
        raise RuntimeError(f"результаты по запросу {query!r} не найдены")
    title, node = found
    tap(node)
    time.sleep(0.5)
    adb("shell", "input", "keyevent", "23")  # DPAD_CENTER: tap only focuses the card
    time.sleep(4.0)
    session = adb("shell", "dumpsys", "media_session", check=False)
    if f"package={VK_PACKAGE}" not in session or "state=3" not in session:
        raise RuntimeError(f"видео не перешло в play: {title}")
    print(f"VK: играет «{title}»")


def power_state() -> bool | None:
    out = adb("shell", "dumpsys", "power", check=False)
    match = re.search(r"mWakefulness=(\w+)", out)
    if not match:
        return None
    return match.group(1) == "Awake"


def set_power(on: bool) -> None:
    current = power_state()
    if current is None:
        raise RuntimeError("не удалось определить состояние экрана")
    if current == on:
        print(f"TV уже {'включён' if on else 'выключен'}")
        return
    # 224 = KEYCODE_WAKEUP, 223 = KEYCODE_SLEEP. Never use the POWER toggle.
    adb("shell", "input", "keyevent", "224" if on else "223")
    time.sleep(2.0)
    after = power_state()
    print(f"TV: {'включён' if after else 'выключен'}")


def volume(action: str, level: int | None) -> None:
    if action == "up":
        adb("shell", "cmd", "media_session", "volume", "--stream", "3", "--adj", "raise")
    elif action == "down":
        adb("shell", "cmd", "media_session", "volume", "--stream", "3", "--adj", "lower")
    elif action == "set":
        if level is None or not 0 <= level <= 100:
            raise RuntimeError("для set нужен уровень 0..100")
        adb(
            "shell",
            "cmd",
            "media_session",
            "volume",
            "--stream",
            "3",
            "--set",
            str(level),
        )
    else:
        raise RuntimeError("volume: up, down или set N")
    out = adb("shell", "cmd", "media_session", "volume", "--stream", "3", "--get")
    match = re.search(r"volume is (\d+) in range \[(\d+)\.\.(\d+)\]", out)
    if match:
        print(f"громкость {match.group(1)} из {match.group(3)}")


def state() -> None:
    on = power_state()
    out = adb("shell", "cmd", "media_session", "volume", "--stream", "3", "--get")
    match = re.search(r"volume is (\d+) in range \[(\d+)\.\.(\d+)\]", out)
    volume_text = f"{match.group(1)}/{match.group(3)}" if match else "?"
    session = adb("shell", "dumpsys", "media_session", check=False)
    playing = "state=3" in session and f"package={VK_PACKAGE}" in session
    print(f"экран={'включён' if on else 'выключен'} громкость={volume_text} vk_playing={playing}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Sber TV control over ADB")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("state")
    power = sub.add_parser("power")
    power.add_argument("action", choices=("on", "off"))
    volume_parser = sub.add_parser("volume")
    volume_parser.add_argument("action", choices=("up", "down", "set"))
    volume_parser.add_argument("level", nargs="?", type=int)
    key = sub.add_parser("key")
    key.add_argument("name", choices=sorted(KEYS))
    vk = sub.add_parser("vk-play")
    vk.add_argument("query")
    args = parser.parse_args()

    connect()
    if args.command == "state":
        state()
    elif args.command == "power":
        set_power(args.action == "on")
    elif args.command == "volume":
        volume(args.action, args.level)
    elif args.command == "key":
        adb("shell", "input", "keyevent", str(KEYS[args.name]))
    elif args.command == "vk-play":
        vk_play(args.query)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.SubprocessError) as error:
        print(f"ошибка: {error}", file=sys.stderr)
        raise SystemExit(1)
