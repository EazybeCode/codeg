#!/usr/bin/env python3
"""Canned codex app-server for the login-dispatch PROOF (not production).

Speaks just enough of the app-server device flow for `CodexLoginDriver` to reach
`awaiting_user`: reply to the initialize request, then emit a device-code challenge,
then idle until the sandbox tears us down. It never contacts OpenAI, so the proof
exercises the REAL LoginChannel (slot proxy + credential deliver + sandbox) and the
supervisor's dispatch without needing real credentials or egress."""

import sys
import time

INIT = b'{"id":1,"result":{"userAgent":"neev-proof"}}\n'
DEVICE = (
    b'{"id":2,"result":{"type":"chatgptDeviceCode","loginId":"proof",'
    b'"verificationUrl":"https://auth.openai.com/codex/device",'
    b'"userCode":"ABCD-EFGH"}}\n'
)


def main():
    out = sys.stdout.buffer
    sys.stdin.buffer.readline()  # the driver's initialize request
    out.write(INIT)
    out.flush()
    sys.stdin.buffer.readline()  # the driver's account/login/start request
    out.write(DEVICE)
    out.flush()
    # Stay alive at awaiting_user until the sandbox SIGKILLs us on cancel.
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            time.sleep(0.2)


if __name__ == "__main__":
    main()
