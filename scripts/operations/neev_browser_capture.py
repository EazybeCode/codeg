#!/usr/bin/python3 -I
"""`$BROWSER` for a sandboxed `claude auth login`.

Claude 2.1.267 hands its loopback-redirect authorize URL ONLY to `$BROWSER`
(measured: it is never printed, and the helper's stdout/stderr are not relayed).
There is no browser in the sandbox, so this records that URL for the supervisor
and exits 0, which is exactly what a browser launcher does.

Writes ONE line to `$NEEV_BROWSER_CAPTURE` (a fixed path in the run workdir the
supervisor created), with O_CREAT|O_EXCL|O_NOFOLLOW, mode 0600. Accepts only the
pinned Claude authorize origin; anything else is ignored. Never prints the URL.
"""

import os
import sys

PREFIX = "https://claude.com/cai/oauth/authorize?"
MAX_BYTES = 2048


def capture(argv, environ) -> int:
    path = environ.get("NEEV_BROWSER_CAPTURE", "")
    if len(argv) != 2 or not path or not os.path.isabs(path):
        return 0
    url = argv[1]
    if (not url.startswith(PREFIX) or len(url) > MAX_BYTES
            or any(ord(ch) < 0x21 or ord(ch) >= 0x7F for ch in url)):
        return 0
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                     | os.O_CLOEXEC, 0o600)
    except OSError:
        return 0
    try:
        os.write(fd, (url + "\n").encode("ascii"))
    finally:
        os.close(fd)
    return 0


if __name__ == "__main__":
    sys.exit(capture(sys.argv, os.environ))
