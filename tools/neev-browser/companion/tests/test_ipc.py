import os
import shutil
import socket
import struct
import tempfile
import unittest

from neev_browser.ipc import AuthError, IpcClient, IpcServer, new_token


class IpcTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.dir, True)
        self.path = os.path.join(self.dir, "ipc.sock")
        self.token = new_token()
        self.server = IpcServer(self.path, self.token,
                                lambda req: {"echo": req.get("v")})
        self.server.start()
        self.addCleanup(self.server.close)

    def test_authenticated_client_round_trips(self):
        client = IpcClient(self.path, self.token, timeout=5)
        self.addCleanup(client.close)
        self.assertEqual(client.call({"v": 42})["echo"], 42)

    def test_wrong_token_is_rejected(self):
        with self.assertRaises((AuthError, socket.timeout, OSError)):
            IpcClient(self.path, "not-the-token", timeout=3)

    def test_a_client_that_skips_the_hello_frame_is_dropped(self):
        raw = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        raw.settimeout(3)
        raw.connect(self.path)
        self.addCleanup(raw.close)
        body = b'{"type":"call","v":1}'
        raw.sendall(struct.pack("<I", len(body)) + body)
        self.assertEqual(raw.recv(4096), b"")

    def test_the_socket_is_a_private_unix_socket_not_a_tcp_port(self):
        self.assertTrue(os.path.exists(self.path))
        self.assertEqual(os.stat(self.path).st_mode & 0o777, 0o600)

    def test_closing_the_server_removes_the_socket(self):
        self.server.close()
        self.assertFalse(os.path.exists(self.path))

    def test_tokens_are_unique_and_long(self):
        self.assertNotEqual(new_token(), new_token())
        self.assertGreaterEqual(len(new_token()), 32)


if __name__ == "__main__":
    unittest.main()
