import json
import unittest

from neev_browser.bounds import MAX_NODES
from neev_browser.ipc import AuthError
from neev_browser.mcp_server import (MAX_TYPE_CHARS, TOOL_SCHEMAS, McpServer,
                                     PROTOCOL_VERSION)
from neev_browser.scope import ALLOWED_TOOLS


class FakeClient(object):
    def __init__(self, reply=None, raises=None):
        self.reply = reply if reply is not None else {}
        self.raises = raises
        self.calls = []

    def call(self, payload):
        self.calls.append(payload)
        if self.raises is not None:
            raise self.raises
        return self.reply


def request(method, params=None, rid=1):
    return {"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}}


def server(reply=None, raises=None):
    client = FakeClient(reply, raises)
    return McpServer(None, None, client), client


def payload_of(response):
    return json.loads(response["result"]["content"][0]["text"])


class HandshakeTest(unittest.TestCase):
    def test_initialize_advertises_tools(self):
        result = server()[0].handle(request("initialize"))["result"]
        self.assertEqual(result["protocolVersion"], PROTOCOL_VERSION)
        self.assertIn("tools", result["capabilities"])
        self.assertEqual(result["serverInfo"]["name"], "neev_browser")

    def test_an_unknown_method_is_a_jsonrpc_error_not_a_crash(self):
        response = server()[0].handle(request("resources/read"))
        self.assertEqual(response["error"]["code"], -32601)


class SurfaceTest(unittest.TestCase):
    def test_tools_list_exposes_exactly_the_allowlist(self):
        names = [t["name"] for t in
                 server()[0].handle(request("tools/list"))["result"]["tools"]]
        self.assertEqual(sorted(names), sorted(ALLOWED_TOOLS))

    def test_no_schema_offers_code_execution_navigation_or_storage(self):
        blob = json.dumps(TOOL_SCHEMAS).lower()
        for forbidden in ("eval", "execute_script", "cookie", "localstorage",
                          "open_tab", "innerhtml", "browser_navigate"):
            self.assertNotIn(forbidden, blob)

    def test_every_schema_refuses_unexpected_arguments(self):
        for schema in TOOL_SCHEMAS:
            self.assertIs(schema["inputSchema"]["additionalProperties"], False)

    def test_the_type_tool_caps_text_length(self):
        schema = [t for t in TOOL_SCHEMAS if t["name"] == "browser_type"][0]
        self.assertEqual(schema["inputSchema"]["properties"]["text"]["maxLength"],
                         MAX_TYPE_CHARS)

    def test_refs_must_look_like_snapshot_refs(self):
        schema = [t for t in TOOL_SCHEMAS if t["name"] == "browser_click"][0]
        self.assertEqual(schema["inputSchema"]["properties"]["ref"]["pattern"],
                         "^e[0-9]+:[0-9]+$")


class CallTest(unittest.TestCase):
    def test_an_unknown_tool_is_refused_without_touching_the_socket(self):
        srv, client = server()
        response = srv.handle(request("tools/call", {"name": "browser_eval",
                                                     "arguments": {"code": "1"}}))
        self.assertTrue(response["result"]["isError"])
        self.assertEqual(client.calls, [])

    def test_oversized_typing_is_refused_before_the_socket(self):
        srv, client = server()
        response = srv.handle(request("tools/call", {
            "name": "browser_type",
            "arguments": {"ref": "e1:1", "text": "x" * (MAX_TYPE_CHARS + 1)}}))
        self.assertTrue(response["result"]["isError"])
        self.assertEqual(client.calls, [])

    def test_a_host_error_becomes_a_tool_error_not_a_protocol_crash(self):
        srv, _ = server({"error": "run_not_active", "message": "run is stopped"})
        result = srv.handle(request("tools/call", {"name": "browser_snapshot"}))["result"]
        self.assertTrue(result["isError"])
        self.assertIn("run_not_active", result["content"][0]["text"])

    def test_a_dead_connection_becomes_a_tool_error(self):
        srv, _ = server(raises=AuthError("closed"))
        result = srv.handle(request("tools/call", {"name": "browser_snapshot"}))["result"]
        self.assertTrue(result["isError"])

    def test_the_client_never_sees_a_call_with_bad_arguments(self):
        bad = [
            ("browser_click", {}),
            ("browser_click", {"ref": "button.save"}),
            ("browser_click", {"ref": "e1:1", "force": True}),
            ("browser_click", {"ref": 17}),
            ("browser_type", {"ref": "e1:1"}),
            ("browser_type", {"ref": "e1:1", "text": 42}),
            ("browser_type", {"ref": "e1:1", "text": None}),
            ("browser_type", {"ref": "e1:1", "text": "x" * (MAX_TYPE_CHARS + 1)}),
            ("browser_scroll", {}),
            ("browser_scroll", {"direction": "sideways"}),
            ("browser_scroll", {"direction": "down", "amount": True}),
            ("browser_scroll", {"direction": "down", "amount": 0}),
            ("browser_scroll", {"direction": "down", "amount": 99}),
            ("browser_scroll", {"direction": "down", "amount": "3"}),
            ("browser_snapshot", {"selector": "body"}),
            ("browser_screenshot", {"fullPage": True}),
            ("browser_request_user_action", {"reason": "x"}),
            ("browser_request_user_action", {"reason": "", "instruction": "y"}),
            ("browser_request_user_action", {"reason": "x" * 301, "instruction": "y"}),
        ]
        for name, args in bad:
            srv, client = server()
            response = srv.handle(request("tools/call", {"name": name,
                                                         "arguments": args}))
            self.assertTrue(response["result"]["isError"],
                            "%s %r was accepted" % (name, args))
            self.assertEqual(client.calls, [], "%s %r reached the socket" % (name, args))

    def test_non_object_arguments_are_refused(self):
        for args in ("ref=e1:1", ["e1:1"], 7):
            srv, client = server()
            response = srv.handle(request("tools/call", {"name": "browser_click",
                                                         "arguments": args}))
            self.assertTrue(response["result"]["isError"])
            self.assertEqual(client.calls, [])

    def test_valid_arguments_do_reach_the_socket(self):
        good = [("browser_snapshot", {}),
                ("browser_screenshot", {}),
                ("browser_click", {"ref": "e12:3"}),
                ("browser_type", {"ref": "e12:3", "text": ""}),
                ("browser_scroll", {"direction": "down", "amount": 3}),
                ("browser_scroll", {"ref": "e12:3"}),
                ("browser_request_user_action", {"reason": "r", "instruction": "i"})]
        for name, args in good:
            srv, client = server()
            srv.handle(request("tools/call", {"name": name, "arguments": args}))
            self.assertEqual(len(client.calls), 1, "%s %r was blocked" % (name, args))

    def test_a_shapeless_reply_is_reported_as_a_failure_not_a_success(self):
        cases = [
            ("browser_snapshot", {}),
            ("browser_snapshot", {"ok": True}),
            ("browser_click", {"snapshot": {"epoch": 1, "nodes": []}}),
            ("browser_click", {"ok": True, "snapshot": {"epoch": 1, "nodes": []}}),
            ("browser_click", {"ok": True, "approval": "maybe",
                               "snapshot": {"epoch": 1, "nodes": []}}),
            ("browser_click", {"ok": "yes", "approval": "approved",
                               "snapshot": {"epoch": 1, "nodes": []}}),
            ("browser_scroll", {"snapshot": {"epoch": 1, "nodes": []}}),
            ("browser_request_user_action",
             {"snapshot": {"epoch": 1, "nodes": []}}),
            ("browser_request_user_action",
             {"outcome": "pending", "snapshot": {"epoch": 1, "nodes": []}}),
        ]
        for name, reply in cases:
            srv, _ = server(reply)
            args = {"browser_click": {"ref": "e1:1"},
                    "browser_scroll": {"direction": "down"},
                    "browser_request_user_action": {"reason": "r",
                                                    "instruction": "i"}}.get(name, {})
            response = srv.handle(request("tools/call", {"name": name,
                                                         "arguments": args}))
            self.assertTrue(response["result"]["isError"],
                            "%s accepted reply %r" % (name, reply))

    def test_a_non_dict_reply_is_a_failure(self):
        srv, _ = server()
        srv._client.reply = ["snapshot"]
        self.assertTrue(srv.handle(request("tools/call",
                                           {"name": "browser_snapshot"}))["result"]["isError"])

    def test_an_action_result_carries_a_fresh_snapshot(self):
        srv, _ = server({"ok": True, "approval": "not_required",
                         "snapshot": {"epoch": 3, "url": "https://a.test/",
                                      "title": "T",
                                      "nodes": [{"ref": "e3:1", "role": "button",
                                                 "name": "Go"}]}})
        payload = payload_of(srv.handle(request("tools/call", {
            "name": "browser_click", "arguments": {"ref": "e3:1"}})))
        self.assertTrue(payload["ok"])
        self.assertEqual(payload["approval"], "not_required")
        self.assertEqual(payload["snapshot"]["nodes"][0]["name"], "Go")

    def test_a_denied_approval_is_a_normal_result_the_model_can_adapt_to(self):
        srv, _ = server({"ok": False, "approval": "denied",
                         "snapshot": {"epoch": 3, "url": "https://a.test/",
                                      "title": "T", "nodes": []}})
        response = srv.handle(request("tools/call", {"name": "browser_click",
                                                     "arguments": {"ref": "e3:1"}}))
        self.assertFalse(response["result"]["isError"])
        self.assertEqual(payload_of(response)["approval"], "denied")

    def test_snapshots_are_bounded_and_redacted_on_the_way_out(self):
        nodes = [{"ref": "e1:%d" % i, "role": "textbox", "name": "n" * 500,
                  "value": "secret-value"} for i in range(400)]
        nodes.append({"ref": "e1:999", "role": "password", "name": "Password",
                      "value": "hunter2"})
        srv, _ = server({"snapshot": {"epoch": 1, "url": "https://a.test/",
                                      "title": "T", "nodes": nodes,
                                      "cookies": "sid=abc"}})
        text = srv.handle(request("tools/call",
                                  {"name": "browser_snapshot"}))["result"]["content"][0]["text"]
        for leaked in ("secret-value", "hunter2", "sid=abc"):
            self.assertNotIn(leaked, text)
        self.assertEqual(len(json.loads(text)["snapshot"]["nodes"]), MAX_NODES)

    def test_a_screenshot_comes_back_as_an_image_part(self):
        srv, _ = server({"format": "jpeg", "dataBase64": "AAAA"})
        content = srv.handle(request("tools/call",
                                     {"name": "browser_screenshot"}))["result"]["content"]
        self.assertEqual(content[0]["type"], "image")
        self.assertEqual(content[0]["mimeType"], "image/jpeg")

    def test_an_oversized_screenshot_is_refused_rather_than_returned(self):
        srv, _ = server({"dataBase64": "A" * 600001})
        self.assertTrue(srv.handle(request("tools/call",
                                           {"name": "browser_screenshot"}))["result"]["isError"])

    def test_a_user_action_outcome_is_passed_through(self):
        srv, _ = server({"outcome": "skipped",
                         "snapshot": {"epoch": 2, "url": "https://a.test/",
                                      "title": "T", "nodes": []}})
        payload = payload_of(srv.handle(request("tools/call", {
            "name": "browser_request_user_action",
            "arguments": {"reason": "sign in", "instruction": "log in please"}})))
        self.assertEqual(payload["outcome"], "skipped")

    def test_unlisted_reply_keys_are_not_forwarded_to_the_model(self):
        srv, _ = server({"ok": True, "cookies": "sid=abc", "html": "<b>x</b>",
                         "snapshot": {"epoch": 1, "url": "https://a.test/",
                                      "title": "T", "nodes": []}})
        text = srv.handle(request("tools/call", {
            "name": "browser_scroll",
            "arguments": {"direction": "down"}}))["result"]["content"][0]["text"]
        self.assertNotIn("sid=abc", text)
        self.assertNotIn("<b>x</b>", text)
        self.assertEqual(sorted(json.loads(text)), ["ok", "snapshot"])


class StdioTest(unittest.TestCase):
    def _run(self, payload):
        import io
        out = io.BytesIO()
        srv = McpServer(io.BufferedReader(io.BytesIO(payload)), out, FakeClient())
        srv.serve_forever()
        return [json.loads(line) for line in out.getvalue().splitlines() if line]

    def test_requests_are_answered_in_order(self):
        payload = (json.dumps(request("initialize", rid=1)).encode() + b"\n"
                   + json.dumps(request("tools/list", rid=2)).encode() + b"\n")
        self.assertEqual([r["id"] for r in self._run(payload)], [1, 2])

    def test_an_over_long_line_is_discarded_rather_than_parsed(self):
        giant = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                            "params": {"name": "browser_type",
                                       "arguments": {"ref": "e1:1",
                                                     "text": "x" * 400000}}})
        payload = (giant.encode() + b"\n"
                   + json.dumps(request("tools/list", rid=2)).encode() + b"\n")
        responses = self._run(payload)
        self.assertEqual([r["id"] for r in responses], [2])

    def test_unparseable_and_notification_lines_are_skipped(self):
        payload = (b"not json\n"
                   + json.dumps({"jsonrpc": "2.0",
                                 "method": "notifications/initialized"}).encode() + b"\n"
                   + b"\n"
                   + json.dumps(request("ping", rid=9)).encode() + b"\n")
        self.assertEqual([r["id"] for r in self._run(payload)], [9])


if __name__ == "__main__":
    unittest.main()
