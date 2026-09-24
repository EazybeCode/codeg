"""Whole-companion tests with a real child process as the agent.

These exercise the path a live run takes — launcher script, MCP stdio server,
unix socket, host scope guard, tool relay, output streaming — with
`tests/fake_agent.py` standing in for the provider CLI. No provider is
installed, called, or paid.
"""

import json
import os
import shutil
import tempfile
import threading
import time
import unittest

from neev_browser.host import Host
from neev_browser.scope import make_ref
from tests.fakes import RecordingOut

FAKE_AGENT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fake_agent.py")
TARGET = {"tabId": 5, "url": "https://app.test/page"}


def snapshot(epoch, nodes=None):
    return {"epoch": epoch, "url": "https://app.test/page", "title": "Page",
            "nodes": nodes if nodes is not None else
            [{"ref": make_ref(epoch, 1), "role": "button", "name": "Save",
              "consequential": True},
             {"ref": make_ref(epoch, 2), "role": "password", "name": "Password",
              "value": "hunter2"}]}


class FakeExtension(object):
    """Answers the host's tool.call frames the way the side panel would."""

    def __init__(self, host, out, responder):
        self._host = host
        self._out = out
        self._responder = responder
        self._seen = 0
        self._stop = threading.Event()
        self.calls = []
        self.epoch = 0

    def start(self):
        thread = threading.Thread(target=self._loop)
        thread.daemon = True
        thread.start()
        self._thread = thread

    def _loop(self):
        while not self._stop.is_set():
            messages = self._out.messages()
            for message in messages[self._seen:]:
                self._seen += 1
                if message.get("type") == "tool.call":
                    self.calls.append(message)
                    self._answer(message)
                elif message.get("type") == "run.ended":
                    self._stop.set()
            time.sleep(0.01)

    def _answer(self, call):
        reply = self._responder(self, call)
        payload = {"type": "tool.result", "runId": call["runId"],
                   "callId": call["callId"], "url": "https://app.test/page"}
        payload.update(reply)
        self._host.handle(payload)

    def stop(self):
        self._stop.set()


def default_responder(extension, call):
    if call["tool"] == "browser_snapshot":
        extension.epoch += 1
        return {"result": {"snapshot": snapshot(extension.epoch)}}
    if call["tool"] in ("browser_click", "browser_type"):
        extension.epoch += 1
        return {"result": {"ok": True, "approval": "approved",
                           "snapshot": snapshot(extension.epoch)}}
    return {"result": {"ok": True}}


class EndToEndTestCase(unittest.TestCase):
    def setUp(self):
        self.base = tempfile.mkdtemp()
        os.chmod(self.base, 0o700)
        self.addCleanup(shutil.rmtree, self.base, True)
        self.out = RecordingOut()
        self.host = Host(self.out, base_dir=self.base, call_timeout=20.0)
        self.addCleanup(self.host.shutdown)
        self.extension = None

    def run_agent(self, provider="claude", script=None, responder=default_responder,
                  run_id="r1"):
        os.environ["NEEV_BROWSER_%s_BIN" % provider.upper()] = FAKE_AGENT
        self.addCleanup(os.environ.pop, "NEEV_BROWSER_%s_BIN" % provider.upper(), None)
        if script is not None:
            os.environ["FAKE_AGENT_SCRIPT"] = json.dumps(script)
            self.addCleanup(os.environ.pop, "FAKE_AGENT_SCRIPT", None)
        self.extension = FakeExtension(self.host, self.out, responder)
        self.extension.start()
        self.addCleanup(self.extension.stop)
        thread = self.host.handle({"type": "run.start", "runId": run_id,
                                   "provider": provider, "task": "do the thing",
                                   "target": dict(TARGET)})
        thread.join(timeout=30)
        return run_id

    def wait_for(self, kind, timeout=30):
        deadline = time.time() + timeout
        while time.time() < deadline:
            found = [m for m in self.out.messages() if m.get("type") == kind]
            if found:
                return found
            time.sleep(0.02)
        self.fail("no %s message arrived within %ss" % (kind, timeout))

    def agent_text(self):
        return "\n".join(m["text"] for m in self.out.messages()
                         if m.get("type") == "run.output")


class LiveRunTest(EndToEndTestCase):
    def test_the_agent_reaches_the_browser_tools_through_the_whole_stack(self):
        self.run_agent(script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.wait_for("run.ended")
        text = self.agent_text()
        self.assertIn("browser_snapshot ->", text)
        self.assertEqual([c["tool"] for c in self.extension.calls],
                         ["browser_snapshot"])

    def test_the_agent_sees_exactly_the_six_allowlisted_tools(self):
        self.run_agent(script=[])
        self.wait_for("run.ended")
        listed = [line for line in self.agent_text().splitlines()
                  if line.startswith("tools: ")][0]
        self.assertEqual(
            listed,
            "tools: browser_click,browser_request_user_action,browser_screenshot,"
            "browser_scroll,browser_snapshot,browser_type")

    def test_real_stdout_is_streamed_to_the_panel_and_written_to_the_run_log(self):
        self.run_agent(script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.wait_for("run.ended")
        self.assertIn("fake agent finished", self.agent_text())
        log = os.path.join(self.base, "r1", "agent.stdout.log")
        with open(log) as handle:
            raw = handle.read()
        self.assertIn("last_agent_message", raw)

    def test_the_transcript_records_the_calls_and_the_ending(self):
        self.run_agent(script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.wait_for("run.ended")
        with open(os.path.join(self.base, "r1", "transcript.jsonl")) as handle:
            kinds = [json.loads(line)["kind"] for line in handle if line.strip()]
        self.assertIn("tool.call", kinds)
        self.assertIn("run.ended", kinds)

    def test_no_field_value_survives_the_round_trip_to_the_agent(self):
        self.run_agent(script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.wait_for("run.ended")
        self.assertNotIn("hunter2", self.agent_text())
        self.assertNotIn("Password", self.agent_text())

    def test_the_codex_adapter_drives_the_same_stack(self):
        self.run_agent(provider="codex",
                       script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.wait_for("run.ended")
        self.assertEqual([c["tool"] for c in self.extension.calls],
                         ["browser_snapshot"])

    def test_the_run_credential_is_destroyed_but_the_outputs_are_kept(self):
        self.run_agent(script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.wait_for("run.ended")
        root = os.path.join(self.base, "r1")
        self.assertFalse(os.path.exists(os.path.join(root, "agent-token")))
        self.assertFalse(os.path.exists(os.path.join(root, "ipc.sock")))
        self.assertTrue(os.path.exists(os.path.join(root, "transcript.jsonl")))
        self.assertTrue(os.path.exists(os.path.join(root, "mcp-config.json")))
        self.assertTrue(os.path.isdir(os.path.join(root, "files")))


class RefusalTest(EndToEndTestCase):
    def test_a_tool_outside_the_allowlist_is_refused_at_the_mcp_layer(self):
        self.run_agent(script=[{"tool": "browser_eval", "arguments": {"code": "1"}}])
        self.wait_for("run.ended")
        self.assertIn("browser_eval -> ERROR", self.agent_text())
        self.assertEqual(self.extension.calls, [])

    def test_a_stale_ref_is_refused_before_it_reaches_the_page(self):
        self.run_agent(script=[
            {"tool": "browser_snapshot", "arguments": {}},
            {"tool": "browser_click", "arguments": {"ref": "e99:1"}}])
        self.wait_for("run.ended")
        self.assertIn("stale_snapshot", self.agent_text())
        self.assertEqual([c["tool"] for c in self.extension.calls],
                         ["browser_snapshot"])

    def test_a_denied_approval_comes_back_as_a_usable_result(self):
        def deny(extension, call):
            # Even a refused action returns a freshly read page, so it carries
            # a new epoch like any other snapshot.
            extension.epoch += 1
            if call["tool"] == "browser_snapshot":
                return {"result": {"snapshot": snapshot(extension.epoch)}}
            return {"result": {"ok": False, "approval": "denied",
                               "snapshot": snapshot(extension.epoch)}}

        self.run_agent(responder=deny, script=[
            {"tool": "browser_snapshot", "arguments": {}},
            {"tool": "browser_click", "arguments": {"ref": "e1:1"}}])
        self.wait_for("run.ended")
        text = self.agent_text()
        self.assertIn('"approval": "denied"', text)
        self.assertNotIn("browser_click -> ERROR", text)

    def test_a_result_from_a_drifted_origin_revokes_the_run(self):
        def drift(extension, call):
            extension.epoch += 1
            return {"url": "https://evil.test/page",
                    "result": {"snapshot": snapshot(extension.epoch)}}

        self.run_agent(responder=drift,
                       script=[{"tool": "browser_snapshot", "arguments": {}}])
        ended = self.wait_for("run.ended")
        self.assertEqual(ended[0]["state"], "revoked")
        self.assertEqual(ended[0]["reason"], "origin_drift")


class StopTest(EndToEndTestCase):
    def test_stop_kills_the_agent_and_its_mcp_grandchild(self):
        holding = threading.Event()

        def hold(extension, call):
            holding.set()
            time.sleep(30)                    # never actually answers
            return {"result": {}}

        self.run_agent(responder=hold,
                       script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.assertTrue(holding.wait(30), "the agent never reached a tool call")
        process = self.host._procs["r1"]
        pgid = process.pgid
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.wait_for("run.ended")
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                os.killpg(pgid, 0)
            except OSError:
                return
            time.sleep(0.1)
        self.fail("the agent's process group survived Stop")

    def test_after_stop_no_further_call_reaches_the_page(self):
        holding = threading.Event()

        def hold(extension, call):
            holding.set()
            time.sleep(30)
            return {"result": {}}

        self.run_agent(responder=hold,
                       script=[{"tool": "browser_snapshot", "arguments": {}}])
        self.assertTrue(holding.wait(30))
        before = len(self.extension.calls)
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.wait_for("run.ended")
        self.assertEqual(
            self.host.tool_call("r1", "browser_snapshot", {})["error"],
            "run_not_active")
        time.sleep(0.5)
        self.assertEqual(len(self.extension.calls), before)


if __name__ == "__main__":
    unittest.main()
