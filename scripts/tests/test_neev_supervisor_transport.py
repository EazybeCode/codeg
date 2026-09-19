"""Portable framing/handshake/channel/deadline tests for the supervisor transport.

Linux `SO_PEERCRED`/filesystem acceptance is NOT exercised here (this host is
darwin); the production `adopt` returns `unsupported` and is asserted as such.
The metadata policy is tested with invented snapshots, and the framing/handshake/
channel/deadline logic over a real pre-bound AF_UNIX socket via the test-only
`_adopt_unchecked` seam.
"""

import asyncio
import json
import os
import platform
import socket
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_supervisor_dto as dto  # noqa: E402
import neev_supervisor_transport as tp  # noqa: E402
import neev_supervisor_wire as wire  # noqa: E402

FIXTURE = json.loads(
    (Path(__file__).parent / "fixtures" / "neev-control-dto-v1.json").read_text(
        encoding="utf-8"
    )
)
REQUESTS = {i["name"]: bytes.fromhex(i["payload_hex"]) for i in FIXTURE["requests"]}
RESPONSES = {i["name"]: bytes.fromhex(i["payload_hex"]) for i in FIXTURE["responses"]}
KEY = bytes(range(32))


def _correlated_pair():
    """A fixture (request_payload, response_payload) whose request has sequence 1."""
    by_request = {}
    for item in FIXTURE["responses"]:
        by_request.setdefault(item.get("request_name"), item["name"])
    for item in FIXTURE["requests"]:
        request = dto.decode_request(bytes.fromhex(item["payload_hex"]))
        if int(request.sequence.value) == 1 and item["name"] in by_request:
            return (
                bytes.fromhex(item["payload_hex"]),
                RESPONSES[by_request[item["name"]]],
            )
    raise RuntimeError("no sequence-1 correlated fixture pair")


def _bound_listener_socket(directory):
    path = os.path.join(directory, "control.sock")
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.bind(path)
    sock.listen(32)
    sock.setblocking(False)
    return sock, path


async def _recv_exact(loop, sock, count):
    buffer = bytearray()
    while len(buffer) < count:
        chunk = await loop.sock_recv(sock, count - len(buffer))
        if not chunk:
            raise AssertionError("unexpected client EOF")
        buffer.extend(chunk)
    return bytes(buffer)


async def _recv_frame(loop, sock):
    header = await _recv_exact(loop, sock, 4)
    declared = int.from_bytes(header, "big")
    body = await _recv_exact(loop, sock, declared + 32)
    return header + body


async def _client_handshake(loop, path, key, client_nonce=b"n" * 32):
    conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    conn.setblocking(False)
    await loop.sock_connect(conn, path)
    await loop.sock_sendall(conn, wire.encode_client_hello(client_nonce))
    server_hello = await _recv_exact(loop, conn, 120)
    context = wire.decode_server_hello(key, client_nonce, server_hello)
    return conn, context


class SupervisorTransportTests(unittest.TestCase):
    def test_happy_exchange_round_trips_a_correlated_response(self):
        request_payload, response_payload = _correlated_pair()

        async def scenario():
            loop = asyncio.get_event_loop()
            with tempfile.TemporaryDirectory() as directory:
                sock, path = _bound_listener_socket(directory)
                listener = tp.SupervisorListener._adopt_unchecked(sock, KEY)

                async def client():
                    conn, context = await _client_handshake(loop, path, KEY)
                    await loop.sock_sendall(
                        conn, wire.encode_request(KEY, context, request_payload)
                    )
                    frame = await _recv_frame(loop, conn)
                    payload = wire.decode_response(
                        KEY, context, request_payload, frame
                    )
                    conn.close()
                    return dto.decode_response(payload)

                client_task = asyncio.ensure_future(client())
                session = await listener.accept()
                pending = await session.receive()
                self.assertEqual(
                    pending.coordinator_candidate, pending.request.coordinator
                )
                response = replace(
                    dto.decode_response(response_payload),
                    supervisor_boot_id=listener.supervisor_boot_id,
                )
                await session.respond(pending, response)
                decoded = await client_task
                self.assertEqual(
                    decoded.supervisor_boot_id, listener.supervisor_boot_id
                )
                self.assertEqual(decoded.request_id, pending.request.request_id)
                session.close()
                listener.close()

        asyncio.run(scenario())

    def test_wrong_key_request_is_auth(self):
        request_payload, _ = _correlated_pair()

        async def scenario():
            loop = asyncio.get_event_loop()
            with tempfile.TemporaryDirectory() as directory:
                sock, path = _bound_listener_socket(directory)
                listener = tp.SupervisorListener._adopt_unchecked(sock, KEY)

                async def client():
                    conn, context = await _client_handshake(loop, path, KEY)
                    # Sign the request with the wrong key: MAC verification fails.
                    forged = wire.encode_request(bytes(range(1, 33)), context, request_payload)
                    await loop.sock_sendall(conn, forged)
                    conn.close()

                client_task = asyncio.ensure_future(client())
                session = await listener.accept()
                with self.assertRaises(tp.TransportError) as caught:
                    await session.receive()
                self.assertEqual(caught.exception.category, "auth")
                await client_task
                listener.close()

        asyncio.run(scenario())

    def test_zero_length_frame_is_invalid_before_body(self):
        async def scenario():
            loop = asyncio.get_event_loop()
            with tempfile.TemporaryDirectory() as directory:
                sock, path = _bound_listener_socket(directory)
                listener = tp.SupervisorListener._adopt_unchecked(sock, KEY)

                async def client():
                    conn, _ = await _client_handshake(loop, path, KEY)
                    await loop.sock_sendall(conn, (0).to_bytes(4, "big"))
                    conn.close()

                client_task = asyncio.ensure_future(client())
                session = await listener.accept()
                with self.assertRaises(tp.TransportError) as caught:
                    await session.receive()
                self.assertEqual(caught.exception.category, "invalid")
                await client_task
                listener.close()

        asyncio.run(scenario())

    def test_handshake_timeout_closes_session(self):
        original = tp._HANDSHAKE_DEADLINE
        tp._HANDSHAKE_DEADLINE = 0.2
        try:

            async def scenario():
                loop = asyncio.get_event_loop()
                with tempfile.TemporaryDirectory() as directory:
                    sock, path = _bound_listener_socket(directory)
                    listener = tp.SupervisorListener._adopt_unchecked(sock, KEY)
                    conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    conn.setblocking(False)
                    await loop.sock_connect(conn, path)
                    # Never send a client hello: the handshake read times out.
                    with self.assertRaises(tp.TransportError) as caught:
                        await listener.accept()
                    self.assertEqual(caught.exception.category, "timeout")
                    conn.close()
                    listener.close()

            asyncio.run(scenario())
        finally:
            tp._HANDSHAKE_DEADLINE = original

    def test_session_capacity_is_bounded(self):
        async def scenario():
            with tempfile.TemporaryDirectory() as directory:
                sock, _ = _bound_listener_socket(directory)
                listener = tp.SupervisorListener._adopt_unchecked(sock, KEY)
                # Fill to the fixed limit; the next accept sheds load as Busy
                # before it ever blocks on the socket.
                for index in range(tp._MAX_SESSIONS):
                    listener._sessions.add(("sentinel", index))
                with self.assertRaises(tp.TransportError) as caught:
                    await listener.accept()
                self.assertEqual(caught.exception.category, "busy")
                listener._sessions.clear()
                listener.close()

        asyncio.run(scenario())

    # ---- metadata policy (invented snapshots; no real filesystem) ----

    def _parent(self):
        return tp.ProtectedParent(
            absolute_path="/var/lib/neev-native/control",
            identity=tp.FileIdentity(device=2049, inode=1000),
            gid=800,
            mode=0o750,
        )

    def test_protected_parent_accepts_exact_and_rejects_drift(self):
        parent = self._parent()
        good = tp.MetaSnapshot(uid=0, gid=800, mode=0o750, device=2049, inode=1000, nlink=2)
        tp.check_protected_parent(parent, good)  # no raise
        for bad in [
            replace(good, uid=1000),  # not root
            replace(good, gid=801),  # wrong gid
            replace(good, inode=1001),  # wrong identity
            replace(good, mode=0o757),  # world-writable bits
            replace(good, is_symlink=True),  # symlink
        ]:
            with self.assertRaises(tp.TransportError) as caught:
                tp.check_protected_parent(parent, bad)
            self.assertEqual(caught.exception.category, "metadata")

    def test_protected_parent_path_shape(self):
        for bad_path in ["relative/x", "/a/../b", "/a//b", "/a/b/", "/"]:
            with self.assertRaises(tp.TransportError):
                tp.validate_parent_path(bad_path)
        self.assertEqual(
            tp.validate_parent_path("/var/lib/neev-native/control"),
            ["var", "lib", "neev-native", "control"],
        )

    def test_key_file_policy(self):
        deployment = tp.ControlDeployment(
            socket_parent=self._parent(),
            key_parent=self._parent(),
            key_identity=tp.FileIdentity(device=2049, inode=1234),
            control_gid=800,
            self_uid=0,
            self_gid=0,
            peer_uid=1000,
            peer_gid=1000,
            socket_owner_uid=0,
        )
        good = tp.MetaSnapshot(
            uid=0, gid=800, mode=0o100440, device=2049, inode=1234, nlink=1,
            is_regular=True,
        )
        tp.check_key_file(deployment, good, 32)  # no raise
        with self.assertRaises(tp.TransportError):
            tp.check_key_file(deployment, good, 31)  # wrong length
        with self.assertRaises(tp.TransportError):
            tp.check_key_file(deployment, replace(good, nlink=2), 32)
        with self.assertRaises(tp.TransportError):
            tp.check_key_file(deployment, replace(good, mode=0o100640), 32)
        with self.assertRaises(tp.TransportError):
            tp.check_key_file(deployment, replace(good, inode=9), 32)

    def test_peer_credentials_policy(self):
        deployment = tp.ControlDeployment(
            socket_parent=self._parent(),
            key_parent=self._parent(),
            key_identity=tp.FileIdentity(device=2049, inode=1234),
            control_gid=800,
            self_uid=0,
            self_gid=0,
            peer_uid=1000,
            peer_gid=1000,
            socket_owner_uid=0,
        )
        tp.check_peer_credentials(deployment, pid=42, uid=1000, gid=1000)  # ok
        for pid, uid, gid in [(0, 1000, 1000), (42, 1001, 1000), (42, 1000, 1001), (42, None, 1000)]:
            with self.assertRaises(tp.TransportError) as caught:
                tp.check_peer_credentials(deployment, pid, uid, gid)
            self.assertEqual(caught.exception.category, "peer")

    @unittest.skipIf(platform.system() == "Linux", "production adopt runs on Linux")
    def test_production_adopt_is_unsupported_off_linux(self):
        with tempfile.TemporaryDirectory() as directory:
            sock, _ = _bound_listener_socket(directory)
            deployment = tp.ControlDeployment(
                socket_parent=self._parent(),
                key_parent=self._parent(),
                key_identity=tp.FileIdentity(device=2049, inode=1234),
                control_gid=800,
                self_uid=0,
                self_gid=0,
                peer_uid=1000,
                peer_gid=1000,
                socket_owner_uid=0,
            )
            with self.assertRaises(tp.TransportError) as caught:
                tp.SupervisorListener.adopt(sock, deployment)
            self.assertEqual(caught.exception.category, "unsupported")
            sock.close()


if __name__ == "__main__":
    unittest.main()
