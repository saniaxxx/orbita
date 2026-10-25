"""orbita [-c /etc/orbita/config.yaml] [--debug] [--check]"""

from __future__ import annotations

import argparse
import asyncio
import logging
import signal
import sys

from . import __version__
from .app import App
from .config import ConfigError, load


def main() -> int:
    p = argparse.ArgumentParser(prog="orbita", description=__doc__)
    p.add_argument("-c", "--config", default="/etc/orbita/config.yaml")
    p.add_argument("--debug", action="store_true")
    p.add_argument("--check", action="store_true", help="validate the config and exit")
    p.add_argument("--version", action="version", version=f"orbita {__version__}")
    args = p.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.debug else logging.INFO,
        # journald adds its own time stamps.
        format="%(levelname)s %(name)s: %(message)s",
        stream=sys.stderr,
    )
    if not args.debug:
        logging.getLogger("aioesphomeapi").setLevel(logging.WARNING)

    async def run() -> int:
        # App inside the loop: the ESPHome clients need a running one.
        try:
            config = load(args.config)
            app = App(config)
        except (OSError, ConfigError) as e:
            logging.error("config %s: %s", args.config, e)
            return 2
        if args.check:
            print(f"{args.config}: ok, {len(config.satellites)} satellite(s)")
            return 0
        stop = asyncio.Event()
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, stop.set)
        await app.run(stop)
        return 0

    return asyncio.run(run())


if __name__ == "__main__":
    sys.exit(main())
