"""Test-only supervisor-transport peer for Rust interoperability.

NOT an installed launcher or production CLI. It prebinds a fresh temporary
filesystem socket, adopts it via the test-only `_adopt_unchecked` seam, prints a
one-line readiness marker, then performs exactly one receive/respond exchange and
exits. It never provisions a key, unlinks a stale socket or runs any effect.

Usage: python3 -I -B neev_transport_peer.py <socket_path> <key_hex> <response_hex>
"""

import asyncio
import socket
import sys
from dataclasses import replace
from pathlib import Path

OPERATIONS = Path(__file__).parents[2] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_supervisor_dto as dto  # noqa: E402
import neev_supervisor_transport as tp  # noqa: E402


async def main(socket_path, key, response_payload):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.bind(socket_path)
    sock.listen(32)
    sock.setblocking(False)
    listener = tp.SupervisorListener._adopt_unchecked(sock, key)
    sys.stdout.write("READY\n")
    sys.stdout.flush()
    session = await listener.accept()
    try:
        pending = await session.receive()
        response = replace(
            dto.decode_response(response_payload),
            supervisor_boot_id=listener.supervisor_boot_id,
        )
        await session.respond(pending, response)
    finally:
        session.close()
        listener.close()


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1], bytes.fromhex(sys.argv[2]), bytes.fromhex(sys.argv[3])))
