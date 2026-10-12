"""Shared-integration broker: the agent reaches an integration's tools through a
socket, and the credential stays on the broker's side of it.

Everything here runs locally without root: a fake Streamable-HTTP server, a fake
legacy-SSE server, a fake CONNECT proxy, and a stub stdio package. The Linux-only
pieces (abstract sockets, peer uid, namespaces) are covered by their pure parts."""

import hashlib
import http.server
import json
import os
import socket
import socketserver
import sys
import tempfile
import threading
import time
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "operations"))

import neev_mcp_broker as broker  # noqa: E402
import neev_mcp_bridge as bridge  # noqa: E402

SECRET = "ghp_SYNTHETIC_TEST_TOKEN_0123456789abcdef"


def init_request(id_=1):
    return {"jsonrpc": "2.0", "id": id_, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "t", "version": "0"}}}


class FakeMcp(http.server.BaseHTTPRequestHandler):
    """Streamable HTTP at /mcp, legacy SSE at /sse (+ /messages), 404 at /gone,
    500 at /broken. Requires `Authorization: Bearer SECRET`."""

    protocol_version = "HTTP/1.1"
    seen = []
    sse_queues = {}

    def log_message(self, *args):
        pass

    def _auth_ok(self):
        FakeMcp.seen.append((self.command, self.path, dict(self.headers)))
        return self.headers.get("Authorization") == "Bearer " + SECRET

    def _send(self, status, body=b"", ctype="application/json", extra=None):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def _body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"null")

    expire_once = {"armed": False}
    drop_first_stream = {"armed": False}
    lose_reply = {"armed": False}

    def do_POST(self):
        if self.path in ("/bad400", "/legacy400"):
            self._body()
            return self._send(400, b"", "text/plain")
        if self.path == "/github400":
            # api.githubcopilot.com/mcp/ with a token it cannot parse: 400 on
            # the POST and on the GET, never 401.
            FakeMcp.seen.append((self.command, self.path, dict(self.headers)))
            self._body()
            return self._send(400, b"Authorization header is badly formatted\n", "text/plain")
        if self.path == "/invalid_token":
            FakeMcp.seen.append((self.command, self.path, dict(self.headers)))
            self._body()
            return self._send(400, b"", "text/plain", extra={
                "WWW-Authenticate": 'Bearer error="invalid_token", '
                                    'error_description="malformed"'})
        if self.path == "/forbidden":
            self._body()
            return self._send(403, b"", "text/plain")
        if self.path == "/redirect":
            self._body()
            return self._send(302, b"", "text/plain",
                              extra={"Location": "https://evil.example.net/mcp"})
        if self.path == "/gone":
            return self._send(404, b"not here", "text/plain")
        if self.path == "/broken":
            return self._send(500, b"boom", "text/plain")
        if self.path == "/sse":
            # A legacy SSE server does not take POSTs at its stream URL.
            self._body()
            return self._send(405, b"", "text/plain")
        if not self._auth_ok():
            self._body()
            return self._send(401, b'{"error":"bad token ' + SECRET[:6].encode() + b'"}')
        message = self._body()
        if self.path.startswith("/messages"):
            queue = FakeMcp.sse_queues.get(self.path.split("=", 1)[1])
            if message.get("method") == "tools/call" and FakeMcp.lose_reply["armed"]:
                # Accept the call, then drop the stream before any reply.
                FakeMcp.lose_reply["armed"] = False
                FakeMcp.lose_reply["drop"] = True
                return self._send(202, b"", "text/plain")
            if message.get("method") == "tools/call":
                queue.append({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "content": [{"type": "text", "text": "token is " + SECRET}]}})
                return self._send(202, b"", "text/plain")
            if "id" in message:
                queue.append({"jsonrpc": "2.0", "id": message["id"],
                              "result": self._result(message)})
            return self._send(202, b"", "text/plain")
        if "id" not in message:
            return self._send(202, b"", "text/plain")
        if message["method"] == "initialize":
            body = json.dumps({"jsonrpc": "2.0", "id": message["id"],
                               "result": self._result(message)}).encode()
            return self._send(200, body, extra={"Mcp-Session-Id": "sess-1"})
        if FakeMcp.expire_once["armed"] and self.headers.get("Mcp-Session-Id") == "sess-1":
            FakeMcp.expire_once["armed"] = False
            FakeMcp.expire_once["expired"] = True
            return self._send(404, b"", "text/plain")
        if self.headers.get("Mcp-Session-Id") != "sess-1":
            return self._send(400, b"missing session", "text/plain")
        if message["method"] == "tools/call" and FakeMcp.lose_reply["armed"]:
            # A progress notification, then the stream ends with no reply.
            FakeMcp.lose_reply["armed"] = False
            body = ("event: message\ndata: " + json.dumps(
                {"jsonrpc": "2.0", "method": "notifications/progress",
                 "params": {"progressToken": 1, "progress": 1}}) + "\n\n").encode()
            return self._send(200, body, "text/event-stream")
        if message["method"] == "tools/call":
            body = json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": {
                "content": [{"type": "text", "text": "token is " + SECRET}]}}).encode()
            return self._send(200, body)
        # Answer tools/list as an SSE stream with a notification first.
        events = (
            "event: message\ndata: " + json.dumps(
                {"jsonrpc": "2.0", "method": "notifications/message",
                 "params": {"level": "info", "data": "hi"}}) + "\n\n"
            + "event: message\ndata: " + json.dumps(
                {"jsonrpc": "2.0", "id": message["id"], "result": self._result(message)})
            + "\n\n").encode()
        return self._send(200, events, "text/event-stream")

    def _result(self, message):
        if message["method"] == "initialize":
            return {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1"}}
        if message["method"] == "tools/list":
            return {"tools": [{"name": "search", "inputSchema": {"type": "object"}},
                              {"name": "fetch", "inputSchema": {"type": "object"}}]}
        return {}

    def do_GET(self):
        if self.path == "/github400":
            FakeMcp.seen.append((self.command, self.path, dict(self.headers)))
            return self._send(400, b"Authorization header is badly formatted\n", "text/plain")
        if self.path not in ("/sse", "/legacy400"):
            FakeMcp.seen.append((self.command, self.path, dict(self.headers)))
            return self._send(404, b"", "text/plain")
        if not self._auth_ok():
            return self._send(401, b"", "text/plain")
        session = "s%d" % len(FakeMcp.sse_queues)
        queue = FakeMcp.sse_queues[session] = []
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(("event: endpoint\ndata: /messages?sessionId=%s\n\n" % session).encode())
        self.wfile.flush()
        # Optionally end the FIRST stream after one message, like a proxy or
        # server dropping an idle legacy SSE connection.
        drop = FakeMcp.drop_first_stream["armed"]
        FakeMcp.drop_first_stream["armed"] = False
        deadline = time.time() + 10
        while time.time() < deadline:
            if FakeMcp.lose_reply.pop("drop", False):
                self.close_connection = True
                return
            while queue:
                item = queue.pop(0)
                self.wfile.write(("event: message\ndata: %s\n\n" % json.dumps(item)).encode())
                self.wfile.flush()
                if drop:
                    self.close_connection = True
                    return
            time.sleep(0.02)


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def serve(handler):
    server = Server(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


class Client:
    """A newline-JSON MCP client over the broker's socket (what the bridge relays)."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(15)
        self.sock.connect(path)
        self.file = self.sock.makefile("rb")

    def send(self, message):
        self.sock.sendall((json.dumps(message) + "\n").encode())

    def recv(self):
        line = self.file.readline()
        if not line:
            raise EOFError
        return json.loads(line)

    def request(self, message):
        self.send(message)
        while True:
            reply = self.recv()
            if reply.get("id") == message["id"]:
                return reply

    def close(self):
        self.sock.close()


class BrokerHarness(unittest.TestCase):
    def setUp(self):
        FakeMcp.seen = []
        FakeMcp.sse_queues = {}
        FakeMcp.expire_once = {"armed": False}
        FakeMcp.drop_first_stream = {"armed": False}
        FakeMcp.lose_reply = {"armed": False}
        self.http = serve(FakeMcp)
        self.addCleanup(self.http.shutdown)
        self.dir = tempfile.mkdtemp(prefix="nmb")
        port = self.http.server_address[1]
        import http.client

        self.connector = lambda host, port_=None: http.client.HTTPConnection(
            "127.0.0.1", port, timeout=10)

    def start(self, servers, **kw):
        b = broker.Broker(servers, socket_dir=self.dir, connector=self.connector, **kw)
        b.start()
        self.addCleanup(b.stop)
        return b

    def client(self, key):
        c = Client(os.path.join(self.dir, key + ".sock"))
        self.addCleanup(c.close)
        return c


def remote(key, path, token=SECRET):
    return {"key": key, "transport": "remote", "url": "https://api.example.com" + path,
            "hosts": ["api.example.com"], "env": {},
            "headers": {"Authorization": "Bearer " + token}}


class RemoteTest(BrokerHarness):
    def test_streamable_http_session_injects_the_secret_header_upstream_only(self):
        self.start([remote("fireflies", "/mcp")])
        c = self.client("fireflies")
        init = c.request(init_request())
        self.assertEqual(init["result"]["serverInfo"]["name"], "fake")
        c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = c.request({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        self.assertEqual([t["name"] for t in tools["result"]["tools"]], ["search", "fetch"])
        posts = [h for (m, p, h) in FakeMcp.seen if m == "POST"]
        self.assertTrue(all(h["Authorization"] == "Bearer " + SECRET for h in posts))
        self.assertEqual(posts[-1].get("Mcp-Session-Id"), "sess-1")
        self.assertEqual(posts[-1].get("MCP-Protocol-Version") or
                         posts[-1].get("Mcp-Protocol-Version"), "2025-06-18")

    def test_legacy_sse_endpoint_is_reached_by_fallback_not_reported_as_404(self):
        self.start([remote("unblocked", "/sse")])
        c = self.client("unblocked")
        init = c.request(init_request())
        self.assertIn("result", init, init)
        c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = c.request({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        self.assertEqual(len(tools["result"]["tools"]), 2)
        gets = [h for (m, p, h) in FakeMcp.seen if m == "GET"]
        self.assertEqual(gets[0]["Authorization"], "Bearer " + SECRET)

    def assertState(self, reply, state):
        self.assertIn("error", reply, reply)
        self.assertEqual(reply["error"]["data"]["neevState"], state)
        text = json.dumps(reply)
        self.assertNotIn(SECRET, text)
        self.assertNotIn(SECRET[:12], text)

    def test_bad_token_is_auth_failed(self):
        self.start([remote("aimfox", "/mcp", token="wrong-token-value-xyz")])
        reply = self.client("aimfox").request(init_request())
        self.assertState(reply, "auth_failed")
        self.assertNotIn("wrong-token-value-xyz", json.dumps(reply))

    def test_404_is_not_found_never_auth_or_network(self):
        self.start([remote("gone", "/gone")])
        reply = self.client("gone").request(init_request())
        self.assertState(reply, "not_found")
        self.assertIn("404", reply["error"]["message"])

    def test_403_is_forbidden_not_auth_failed(self):
        self.start([remote("aimfox", "/forbidden")])
        reply = self.client("aimfox").request(init_request())
        self.assertState(reply, "forbidden")
        self.assertEqual(reply["error"]["data"]["httpStatus"], 403)

    def test_redirects_are_never_followed(self):
        self.start([remote("redir", "/redirect")])
        reply = self.client("redir").request(init_request())
        self.assertState(reply, "protocol_error")
        self.assertFalse(any("evil" in str(h) for (_m, _p, h) in FakeMcp.seen))

    def test_a_dropped_streamable_session_is_reinitialized_once(self):
        self.start([remote("fireflies", "/mcp")])
        c = self.client("fireflies")
        c.request(init_request())
        c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        FakeMcp.expire_once["armed"] = True
        tools = c.request({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        self.assertIn("result", tools, tools)
        self.assertTrue(FakeMcp.expire_once.get("expired"))
        inits = [h for (m, p, h) in FakeMcp.seen if m == "POST"]
        self.assertGreaterEqual(len(inits), 4)  # init, initialized, 404, re-init...

    def test_a_dropped_legacy_stream_is_reopened_and_the_agent_sees_one_reply(self):
        FakeMcp.drop_first_stream["armed"] = True
        self.start([remote("unblocked", "/sse")])
        c = self.client("unblocked")
        self.assertIn("result", c.request(init_request()))
        c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        time.sleep(0.3)
        tools = c.request({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        self.assertEqual(len(tools["result"]["tools"]), 2)
        gets = [p for (m, p, h) in FakeMcp.seen if m == "GET"]
        self.assertEqual(len(gets), 2)

    def test_the_tunnel_cap_reports_busy(self):
        slots = threading.BoundedSemaphore(1)
        slots.acquire()
        saved = broker.UPSTREAM_WAIT_SECONDS
        broker.UPSTREAM_WAIT_SECONDS = 0.2
        try:
            upstream = broker.RemoteUpstream("https://api.example.com/mcp", {}, self.connector,
                                             tunnels=slots)
            with self.assertRaises(broker.IntegrationError) as caught:
                upstream.exchange(init_request())
        finally:
            broker.UPSTREAM_WAIT_SECONDS = saved
        self.assertEqual(caught.exception.state, "busy")

    def _ready(self, key, path):
        self.start([remote(key, path)])
        c = self.client(key)
        self.assertIn("result", c.request(init_request()))
        c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return c

    def test_a_streamable_reply_lost_mid_stream_is_answered_once_and_never_resent(self):
        c = self._ready("fireflies", "/mcp")
        FakeMcp.lose_reply["armed"] = True
        call = {"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {"name": "search", "arguments": {}}}
        reply = c.request(call)
        self.assertState(reply, "connection_lost")
        self.assertIn("NOT retried", reply["error"]["message"])
        calls = [h for (m, p, h) in FakeMcp.seen if m == "POST"]
        self.assertEqual(sum(1 for (m, p, h) in FakeMcp.seen if m == "POST") , len(calls))
        # exactly one tools/call reached the server
        self.assertEqual(FakeMcp.lose_reply, {"armed": False})

    def test_a_legacy_reply_lost_with_the_stream_is_answered_once(self):
        c = self._ready("unblocked", "/sse")
        FakeMcp.lose_reply["armed"] = True
        reply = c.request({"jsonrpc": "2.0", "id": "c1", "method": "tools/call",
                           "params": {"name": "search", "arguments": {}}})
        self.assertState(reply, "connection_lost")
        self.assertEqual(reply["id"], "c1")

    def test_tool_output_carrying_a_secret_is_redacted_before_the_agent(self):
        for key, path in (("fireflies", "/mcp"), ("unblocked", "/sse")):
            with self.subTest(transport=path):
                c = self._ready(key, path)
                reply = c.request({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                                   "params": {"name": "search", "arguments": {}}})
                text = json.dumps(reply)
                self.assertIn("[redacted]", text)
                self.assertNotIn(SECRET, text)

    def test_a_400_at_initialize_also_falls_back_to_legacy_sse(self):
        self.start([remote("legacy", "/bad400")])
        reply = self.client("legacy").request(init_request())
        # /bad400 has no GET stream either, so the fallback reports not_found —
        # but only AFTER trying it (a GET was made).
        self.assertTrue(any(m == "GET" for (m, p, h) in FakeMcp.seen))
        self.assertState(reply, "not_found")

    def test_a_genuine_legacy_server_that_answers_400_still_connects_over_sse(self):
        self.start([remote("legacy", "/legacy400")])
        c = self.client("legacy")
        self.assertIn("result", c.request(init_request()))
        c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = c.request({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        self.assertEqual(len(tools["result"]["tools"]), 2)

    def test_github_style_400_on_post_and_get_is_auth_failed_not_protocol_error(self):
        self.start([remote("github", "/github400", token="not-a-github-token")])
        reply = self.client("github").request(init_request())
        self.assertState(reply, "auth_failed")
        self.assertEqual(reply["error"]["data"]["httpStatus"], 400)
        self.assertIn("400", reply["error"]["message"])
        self.assertNotIn("401", reply["error"]["message"])
        self.assertNotIn("not-a-github-token", json.dumps(reply))
        self.assertNotIn("badly formatted", json.dumps(reply))

    def test_a_400_invalid_token_challenge_is_auth_failed_without_the_sse_fallback(self):
        self.start([remote("github", "/invalid_token")])
        reply = self.client("github").request(init_request())
        self.assertState(reply, "auth_failed")
        self.assertEqual(reply["error"]["data"]["httpStatus"], 400)
        self.assertFalse(any(m == "GET" for (m, p, h) in FakeMcp.seen))

    def test_400_on_both_transports_without_credentials_is_bad_request(self):
        server = dict(remote("open", "/github400"), headers={})
        self.start([server])
        reply = self.client("open").request(init_request())
        self.assertState(reply, "bad_request")
        self.assertEqual(reply["error"]["data"]["httpStatus"], 400)

    def test_a_plain_400_is_bad_request_never_protocol_error(self):
        error = broker._classify_status(400)
        self.assertEqual((error.state, error.status), ("bad_request", 400))
        self.assertEqual(broker._classify_status(422).state, "bad_request")
        self.assertEqual(broker._classify_status(401).state, "auth_failed")

    def test_5xx_is_upstream_error(self):
        self.start([remote("broken", "/broken")])
        self.assertState(self.client("broken").request(init_request()), "upstream_error")

    def test_unreachable_is_its_own_state(self):
        import http.client

        dead = socket.socket()
        dead.bind(("127.0.0.1", 0))
        port = dead.getsockname()[1]
        dead.close()
        self.connector = lambda host, p=None: http.client.HTTPConnection("127.0.0.1", port, timeout=3)
        self.start([remote("dead", "/mcp")])
        self.assertState(self.client("dead").request(init_request()), "unreachable")

    def test_sse_endpoint_on_another_origin_is_refused(self):
        self.assertIsNone(broker.same_origin_endpoint(
            "https://api.example.com/sse", "https://evil.example.net/messages"))
        self.assertIsNone(broker.same_origin_endpoint(
            "https://api.example.com/sse", "//evil.example.net/x"))
        self.assertEqual(
            broker.same_origin_endpoint("https://api.example.com/sse", "/messages?sessionId=1"),
            "https://api.example.com/messages?sessionId=1")

    def test_a_notification_needs_no_reply_and_ids_are_preserved(self):
        self.start([remote("fireflies", "/mcp")])
        c = self.client("fireflies")
        c.request(init_request(id_="abc"))
        c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        reply = c.request({"jsonrpc": "2.0", "id": "x-7", "method": "tools/list"})
        self.assertEqual(reply["id"], "x-7")


class ProxyTest(unittest.TestCase):
    """The production connector always tunnels through the CONNECT proxy."""

    def _proxy(self, response):
        seen = []
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(4)

        def run():
            while True:
                try:
                    conn, _ = listener.accept()
                except OSError:
                    return
                data = conn.recv(4096)
                seen.append(data.split(b"\r\n")[0])
                conn.sendall(response)
                conn.close()

        threading.Thread(target=run, daemon=True).start()
        self.addCleanup(listener.close)
        return "http://127.0.0.1:%d" % listener.getsockname()[1], seen

    def test_proxy_403_is_egress_denied_and_only_connect_is_sent(self):
        proxy, seen = self._proxy(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
        upstream = broker.RemoteUpstream(
            "https://api.fireflies.ai/mcp", {"Authorization": "Bearer " + SECRET},
            broker.proxy_connector(proxy))
        with self.assertRaises(broker.IntegrationError) as caught:
            upstream.exchange(init_request())
        self.assertEqual(caught.exception.state, "egress_denied")
        self.assertEqual(seen, [b"CONNECT api.fireflies.ai:443 HTTP/1.1"])

    def test_proxy_502_is_unreachable(self):
        proxy, _ = self._proxy(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
        upstream = broker.RemoteUpstream("https://api.fireflies.ai/mcp", {},
                                         broker.proxy_connector(proxy))
        with self.assertRaises(broker.IntegrationError) as caught:
            upstream.exchange(init_request())
        self.assertEqual(caught.exception.state, "unreachable")

    def test_the_tunnel_request_is_accepted_by_the_real_slot_proxy_parser(self):
        import neev_provider_egress as egress

        captured = []
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        self.addCleanup(listener.close)

        def run():
            conn, _ = listener.accept()
            data = b""
            while b"\r\n\r\n" not in data:
                data += conn.recv(4096)
            captured.append(data)
            conn.sendall(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            conn.close()

        threading.Thread(target=run, daemon=True).start()
        connect = broker.proxy_connector("http://127.0.0.1:%d" % listener.getsockname()[1])
        with self.assertRaises(OSError):
            connect("api.fireflies.ai").connect()
        policy = egress.policy_for("claude", "run", extra_hosts=("api.fireflies.ai",))
        self.assertEqual(egress.parse_connect(captured[0], policy), "api.fireflies.ai")

    def test_tls_through_the_tunnel_verifies_the_target_name(self):
        import shutil
        import ssl
        import subprocess

        if not shutil.which("openssl"):
            self.skipTest("openssl not available")
        work = tempfile.mkdtemp(prefix="nmbtls")
        cert, key = os.path.join(work, "c.pem"), os.path.join(work, "k.pem")
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                        "-subj", "/CN=api.example.com", "-addext",
                        "subjectAltName=DNS:api.example.com",
                        "-keyout", key, "-out", cert], check=True, capture_output=True)
        server = serve(FakeMcp)
        self.addCleanup(server.shutdown)
        server_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server_ctx.load_cert_chain(cert, key)
        server.socket = server_ctx.wrap_socket(server.socket, server_side=True)
        upstream_port = server.server_address[1]
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(4)
        self.addCleanup(listener.close)

        def tunnel():
            conn, _ = listener.accept()
            data = b""
            while b"\r\n\r\n" not in data:
                data += conn.recv(4096)
            up = socket.create_connection(("127.0.0.1", upstream_port))
            conn.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")

            def pipe(a, b):
                try:
                    while True:
                        chunk = a.recv(65536)
                        if not chunk:
                            break
                        b.sendall(chunk)
                except OSError:
                    pass
                finally:
                    for s in (a, b):
                        try:
                            s.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass

            threading.Thread(target=pipe, args=(conn, up), daemon=True).start()
            threading.Thread(target=pipe, args=(up, conn), daemon=True).start()

        threading.Thread(target=tunnel, daemon=True).start()
        ctx = ssl.create_default_context(cafile=cert)
        connect = broker.proxy_connector("http://127.0.0.1:%d" % listener.getsockname()[1], ctx)
        upstream = broker.RemoteUpstream("https://api.example.com/mcp",
                                         {"Authorization": "Bearer " + SECRET}, connect)
        out = upstream.exchange(init_request())
        self.assertEqual(out[0]["result"]["serverInfo"]["name"], "fake")

    def test_there_is_no_direct_path_without_a_proxy(self):
        with self.assertRaises(broker.IntegrationError) as caught:
            broker.proxy_connector(None)
        self.assertEqual(caught.exception.state, "egress_not_configured")


STUB = r'''
import hashlib, json, os, sys
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    if m["method"] == "initialize":
        r = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
             "serverInfo": {"name": "stub", "version": "1"}}
    else:
        token = os.environ.get("STUB_TOKEN", "")
        r = {"tools": [{"name": "t_" + hashlib.sha256(token.encode()).hexdigest()[:12],
                         "inputSchema": {"type": "object"}}],
             "env": sorted(k for k in os.environ if k not in ("PWD", "SHLVL", "_", "LC_CTYPE", "__CF_USER_TEXT_ENCODING"))}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}) + "\n")
    sys.stdout.flush()
'''


class PackageTest(BrokerHarness):
    def setUp(self):
        super().setUp()
        self.stub = os.path.join(self.dir, "stub.py")
        with open(self.stub, "w") as handle:
            handle.write(STUB)
        self.packages = {"stub": broker.PackageSpec(
            argv=(sys.executable, "-I", self.stub), env_names=("STUB_TOKEN",))}

    def test_package_child_gets_only_its_declared_secret_and_proxy_env(self):
        self.start([{"key": "hubspot", "transport": "package", "package": "stub",
                     "hosts": ["api.hubspot.com"], "headers": {},
                     "env": {"STUB_TOKEN": SECRET}}],
                   packages=self.packages, proxy="http://10.203.0.2:3128",
                   home=self.dir)
        c = self.client("hubspot")
        self.assertEqual(c.request(init_request())["result"]["serverInfo"]["name"], "stub")
        tools = c.request({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})["result"]
        expected = "t_" + hashlib.sha256(SECRET.encode()).hexdigest()[:12]
        self.assertEqual(tools["tools"][0]["name"], expected)
        self.assertEqual(sorted(tools["env"]), sorted([
            "HOME", "HTTPS_PROXY", "HTTP_PROXY", "LANG", "NODE_USE_ENV_PROXY", "NO_PROXY",
            "PATH", "STUB_TOKEN", "TMPDIR", "https_proxy", "http_proxy", "no_proxy"]))

    def test_fixed_env_always_wins_over_a_secret(self):
        b = broker.Broker([], proxy="http://10.203.0.2:3128", home="/run/neev-mcp/a")
        spec = broker.PackageSpec(argv=("node",), env_names=("PATH", "TOKEN"))
        env = b.package_env({"env": {"PATH": "/evil", "TOKEN": "t"}}, spec)
        self.assertEqual(env["PATH"], "/opt/neev/node/bin:/usr/local/bin:/usr/bin:/bin")
        self.assertEqual(env["TMPDIR"], "/run/neev-mcp/a")
        self.assertEqual(env["NODE_USE_ENV_PROXY"], "1")
        self.assertEqual(env["TOKEN"], "t")
        self.assertEqual(broker.PACKAGES["hubspot"].env_names, ("PRIVATE_APP_ACCESS_TOKEN",))
        self.assertEqual(broker.PACKAGES["notion"].env_names, ("NOTION_API_TOKEN",))

    def test_undeclared_env_is_never_passed(self):
        with self.assertRaises(broker.IntegrationError):
            broker.parse_bundle(json.dumps({"neevIntegrations": 1, "servers": [
                {"key": "hubspot", "transport": "package", "package": "hubspot",
                 "hosts": ["api.hubspot.com"], "headers": {},
                 "env": {"NODE_OPTIONS": "--require /tmp/x.js"}}]}).encode())

    def test_an_oversized_package_line_is_dropped_not_split(self):
        big = os.path.join(self.dir, "big.py")
        with open(big, "w") as handle:
            handle.write("import sys,json\n"
                         "for line in sys.stdin:\n"
                         "    m=json.loads(line)\n"
                         "    sys.stdout.write('x'*(9*1024*1024)+'\\n')\n"
                         "    sys.stdout.write(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{}})+'\\n')\n"
                         "    sys.stdout.flush()\n")
        packages = {"stub": broker.PackageSpec(argv=(sys.executable, "-I", big), env_names=())}
        self.start([{"key": "big", "transport": "package", "package": "stub",
                     "hosts": ["api.example.com"], "headers": {}, "env": {}}],
                   packages=packages, proxy="http://10.203.0.2:3128", home=self.dir)
        c = self.client("big")
        c.sock.settimeout(30)
        self.assertEqual(c.request(init_request()).get("result"), {})

    def test_missing_package_is_package_unavailable(self):
        packages = {"stub": broker.PackageSpec(argv=("/nonexistent/node", "x.js"),
                                               env_names=("STUB_TOKEN",))}
        self.start([{"key": "notion", "transport": "package", "package": "stub",
                     "hosts": ["api.notion.com"], "headers": {}, "env": {}}],
                   packages=packages, proxy="http://10.203.0.2:3128", home=self.dir)
        reply = self.client("notion").request(init_request())
        self.assertEqual(reply["error"]["data"]["neevState"], "package_unavailable")


class BundleTest(unittest.TestCase):
    def bundle(self, **server):
        base = {"key": "fireflies", "transport": "remote", "url": "https://api.fireflies.ai/mcp",
                "hosts": ["api.fireflies.ai"], "env": {}, "headers": {"Authorization": "Bearer x"}}
        base.update(server)
        return json.dumps({"neevIntegrations": 1, "servers": [base]}).encode()

    def test_valid_bundle_parses(self):
        servers = broker.parse_bundle(self.bundle())
        self.assertEqual(servers[0]["key"], "fireflies")

    def test_rejections(self):
        for bad in (
            {"key": "Bad Key"}, {"key": "x" * 25}, {"transport": "shell"},
            {"url": "http://api.fireflies.ai/mcp"}, {"url": "https://other.example.com/mcp"},
            {"url": "https://api.fireflies.ai/mcp?token=1"}, {"hosts": ["10.0.0.1"]},
            {"headers": {"Authorization": "Bearer x\r\nX-Evil: 1"}},
            {"headers": {"Host": "evil"}}, {"headers": {"Bad Name": "v"}},
            {"headers": {"Proxy-Foo": "v"}}, {"headers": {"Cookie": "a=b"}},
            {"package": "hubspot"},
        ):
            with self.subTest(bad=bad):
                with self.assertRaises(broker.IntegrationError):
                    broker.parse_bundle(self.bundle(**bad))
        with self.assertRaises(broker.IntegrationError):
            broker.parse_bundle(b"{" * 10)
        with self.assertRaises(broker.IntegrationError):
            broker.parse_bundle(json.dumps({"neevIntegrations": 2, "servers": []}).encode())
        dup = json.loads(self.bundle())
        dup["servers"].append(dict(dup["servers"][0]))
        with self.assertRaises(broker.IntegrationError):
            broker.parse_bundle(json.dumps(dup).encode())

    def test_non_ascii_secret_values_and_bad_ports_are_invalid_not_crashes(self):
        with self.assertRaises(broker.IntegrationError):
            broker.parse_bundle(self.bundle(headers={"Authorization": "Bearer t\u00e9st"}))
        with self.assertRaises(broker.IntegrationError):
            broker.parse_bundle(self.bundle(url="https://api.fireflies.ai:99999/mcp"))
        with self.assertRaises(broker.IntegrationError):
            broker.parse_bundle(self.bundle(url="https://api.fireflies.ai:abc/mcp"))

    def test_hosts_are_collected_for_the_proxy(self):
        servers = broker.parse_bundle(self.bundle())
        self.assertEqual(broker.bundle_hosts(servers), ("api.fireflies.ai",))

    def test_redaction_removes_every_secret_value(self):
        servers = broker.parse_bundle(self.bundle(headers={"Authorization": "Bearer " + SECRET}))
        text = broker.redact("upstream said Bearer %s and %s" % (SECRET, SECRET[7:]), servers)
        self.assertNotIn(SECRET, text)


class HardeningTest(unittest.TestCase):
    def test_accept_fixture_needs_root_owned_unwritable_files(self):
        import stat

        class Info:
            def __init__(self, uid, mode):
                self.st_uid, self.st_mode = uid, mode

        good = {"/opt/neev-accept": Info(0, stat.S_IFDIR | 0o755),
                "/opt/neev-accept/stub_mcp_server.py": Info(0, stat.S_IFREG | 0o444)}

        def lstat_from(table):
            def lstat(path):
                if path not in table:
                    raise FileNotFoundError(path)
                return table[path]
            return lstat

        spec = broker.accept_fixture_spec(lstat=lstat_from(good))
        self.assertEqual(spec.argv, ("/usr/bin/python3", "-I",
                                     "/opt/neev-accept/stub_mcp_server.py"))
        self.assertEqual(spec.env_names, ("NEEV_ACCEPT_TOKEN",))
        for path, info in (("/opt/neev-accept", Info(61000, stat.S_IFDIR | 0o755)),
                           ("/opt/neev-accept", Info(0, stat.S_IFDIR | 0o777)),
                           ("/opt/neev-accept/stub_mcp_server.py", Info(0, stat.S_IFREG | 0o666)),
                           ("/opt/neev-accept/stub_mcp_server.py", Info(0, stat.S_IFLNK | 0o777))):
            bad = dict(good)
            bad[path] = info
            self.assertIsNone(broker.accept_fixture_spec(lstat=lstat_from(bad)))
        self.assertIsNone(broker.accept_fixture_spec(lstat=lstat_from({})))
        self.assertNotIn(broker.ACCEPT_FIXTURE_PACKAGE, broker.PACKAGES)

    def test_harden_process_makes_the_broker_non_dumpable(self):
        calls = []
        self.assertTrue(broker.harden_process(prctl=lambda *a: calls.append(a) or 0))
        self.assertEqual(calls, [(4, 0, 0, 0, 0)])


class GenerationTest(unittest.TestCase):
    def test_an_old_stream_ending_answers_only_its_own_ids(self):
        replies = []
        upstream = broker.RemoteUpstream("https://api.example.com/sse", {}, None,
                                         on_message=replies.append, key="k")
        upstream._pending = {broker._id_key(1): 1, broker._id_key(2): 2}
        upstream._answer_lost(1)
        self.assertEqual([r["id"] for r in replies], [1])
        self.assertEqual(replies[0]["error"]["data"]["neevState"], "connection_lost")
        self.assertEqual(list(upstream._pending), [broker._id_key(2)])


class PeerTest(unittest.TestCase):
    def test_peer_uid_policy(self):
        self.assertTrue(broker.peer_allowed(61000, 61000))
        self.assertFalse(broker.peer_allowed(61000, 61001))
        self.assertFalse(broker.peer_allowed(61000, None))
        self.assertTrue(broker.peer_allowed(None, None))

    def test_socket_address_is_abstract_in_production(self):
        self.assertEqual(broker.socket_address("github"), "\0neev-mcp/github")
        self.assertEqual(broker.socket_address("github", "/tmp/d"), "/tmp/d/github.sock")


class CheckTest(BrokerHarness):
    def test_check_reports_ready_auth_failed_not_found_and_missing(self):
        import neev_mcp_check as check

        self.start([remote("fireflies", "/mcp"), remote("unblocked", "/sse"),
                    remote("aimfox", "/mcp", token="nope-nope-nope"), remote("gone", "/gone"),
                    remote("github", "/github400", token="bad-paste-value")])
        results = {key: check.check(key, socket_dir=self.dir, connect_timeout=1)
                   for key in ("fireflies", "unblocked", "aimfox", "gone", "github", "absent")}
        self.assertEqual(results["github"]["state"], "auth_failed")
        self.assertEqual(results["github"]["httpStatus"], 400)
        self.assertEqual(results["fireflies"]["state"], "ready")
        self.assertEqual(results["fireflies"]["toolCount"], 2)
        self.assertEqual(results["unblocked"]["state"], "ready")
        self.assertEqual(results["aimfox"]["state"], "auth_failed")
        self.assertEqual(results["aimfox"]["httpStatus"], 401)
        self.assertEqual(results["gone"]["state"], "not_found")
        self.assertEqual(results["absent"]["state"], "broker_unavailable")
        self.assertNotIn(SECRET, json.dumps(results))


class BridgeTest(BrokerHarness):
    def test_bridge_relays_both_directions(self):
        self.start([remote("fireflies", "/mcp")])
        import io

        lines = [json.dumps(init_request()) + "\n",
                 json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n",
                 json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}) + "\n"]
        stdin = io.BytesIO("".join(lines).encode())
        stdout = io.BytesIO()
        rc = bridge.relay("fireflies", stdin, stdout, socket_dir=self.dir, linger=2.0)
        self.assertEqual(rc, 0)
        replies = [json.loads(l) for l in stdout.getvalue().splitlines()]
        self.assertEqual([r.get("id") for r in replies if "id" in r], [1, 2])

    def test_bridge_without_a_broker_answers_requests_with_a_clear_error(self):
        import io

        stdin = io.BytesIO((json.dumps(init_request()) + "\n").encode())
        stdout = io.BytesIO()
        rc = bridge.relay("absent", stdin, stdout, socket_dir=self.dir, connect_timeout=0.3)
        self.assertEqual(rc, 0)
        reply = json.loads(stdout.getvalue().splitlines()[0])
        self.assertEqual(reply["error"]["data"]["neevState"], "broker_unavailable")

    def test_bridge_key_validation(self):
        import io

        with self.assertRaises(SystemExit):
            bridge.main(["../../etc"], io.BytesIO(), io.BytesIO())


if __name__ == "__main__":
    unittest.main()


@unittest.skipUnless(os.environ.get("NEEV_MCP_SMOKE_ROOT") and os.environ.get("NEEV_MCP_SMOKE_NODE"),
                     "set NEEV_MCP_SMOKE_ROOT (npm ci of deploy/mcp-packages) and NEEV_MCP_SMOKE_NODE")
class PinnedPackageSmokeTest(BrokerHarness):
    """The pinned packages, from the lockfile, start under the broker and list tools."""

    def test_hubspot_and_notion_initialize_and_list_tools(self):
        import neev_mcp_check as check

        node, root = os.environ["NEEV_MCP_SMOKE_NODE"], os.environ["NEEV_MCP_SMOKE_ROOT"]
        packages = {name: broker.PackageSpec(
            argv=(node, spec.argv[1].replace(broker.MCP_ROOT, root + "/node_modules")),
            env_names=spec.env_names) for name, spec in broker.PACKAGES.items()}
        self.start([
            {"key": "hubspot", "transport": "package", "package": "hubspot",
             "hosts": ["api.hubspot.com"], "headers": {}, "env": {"PRIVATE_APP_ACCESS_TOKEN": "dummy-token"}},
            {"key": "notion", "transport": "package", "package": "notion",
             "hosts": ["api.notion.com"], "headers": {}, "env": {"NOTION_API_TOKEN": "dummy-token"}},
        ], packages=packages, proxy="http://127.0.0.1:9", home=self.dir)
        for key, minimum in (("hubspot", 10), ("notion", 10)):
            result = check.check(key, socket_dir=self.dir, connect_timeout=2)
            self.assertEqual(result["state"], "ready", result)
            self.assertGreaterEqual(result["toolCount"], minimum)
