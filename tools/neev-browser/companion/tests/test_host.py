import os
import shutil
import tempfile
import threading
import unittest

from neev_browser.host import Host, ProtocolError, validate
from neev_browser.scope import ScopeError, make_ref
from tests.fakes import FakeIpcServer, FakeLauncher, RecordingOut, messages_of

TARGET = {"tabId": 5, "url": "https://app.test/page"}


class ProtocolTest(unittest.TestCase):
    def test_a_valid_run_start_is_accepted(self):
        message = validate({"type": "run.start", "runId": "r1", "provider": "claude",
                            "task": "hello", "target": dict(TARGET)})
        self.assertEqual(message["provider"], "claude")

    def test_unknown_message_types_are_rejected(self):
        for kind in ("run.eval", "tool.call", "", None, "RUN.START"):
            with self.assertRaises(ProtocolError, msg=repr(kind)):
                validate({"type": kind, "runId": "r1"})

    def test_a_non_object_message_is_rejected(self):
        for raw in ("run.start", ["run.start"], None, 7):
            with self.assertRaises(ProtocolError):
                validate(raw)

    def test_unknown_providers_are_rejected(self):
        with self.assertRaises(ProtocolError):
            validate({"type": "run.start", "runId": "r1", "provider": "rm -rf",
                      "task": "x", "target": dict(TARGET)})

    def test_a_missing_target_is_rejected(self):
        with self.assertRaises(ProtocolError):
            validate({"type": "run.start", "runId": "r1", "provider": "claude",
                      "task": "x"})

    def test_a_non_integer_tab_id_is_rejected(self):
        for tab_id in ("3; rm -rf /", 3.5, True, None, [3]):
            with self.assertRaises(ProtocolError, msg=repr(tab_id)):
                validate({"type": "run.start", "runId": "r1", "provider": "claude",
                          "task": "x", "target": {"tabId": tab_id,
                                                  "url": "https://app.test/"}})

    def test_a_non_http_target_is_rejected(self):
        with self.assertRaises(ScopeError):
            validate({"type": "run.start", "runId": "r1", "provider": "claude",
                      "task": "x", "target": {"tabId": 1, "url": "file:///etc/passwd"}})

    def test_the_task_length_is_capped(self):
        with self.assertRaises(ProtocolError):
            validate({"type": "run.start", "runId": "r1", "provider": "claude",
                      "task": "x" * 100000, "target": dict(TARGET)})

    def test_a_run_id_cannot_carry_path_separators(self):
        for run_id in ("../escape", "a/b", "", "x" * 65):
            with self.assertRaises(ProtocolError, msg=run_id):
                validate({"type": "run.stop", "runId": run_id})


class HostTestCase(unittest.TestCase):
    def setUp(self):
        FakeIpcServer.instances = []
        self.base = tempfile.mkdtemp()
        os.chmod(self.base, 0o700)
        self.addCleanup(shutil.rmtree, self.base, True)
        self.out = RecordingOut()
        self.launcher = FakeLauncher()
        self.host = Host(self.out, base_dir=self.base, launcher=self.launcher,
                         ipc_factory=FakeIpcServer, call_timeout=2.0)
        self.addCleanup(self.host.shutdown)

    def start(self, run_id="r1"):
        thread = self.host.handle({"type": "run.start", "runId": run_id,
                                   "provider": "claude", "task": "t",
                                   "target": dict(TARGET)})
        if thread is not None:
            thread.join(timeout=10)

    def snapshot(self, epoch, call_id, url="https://app.test/page"):
        self.host.handle({"type": "tool.result", "runId": "r1", "callId": call_id,
                          "url": url,
                          "result": {"snapshot": {"epoch": epoch, "url": url,
                                                  "title": "T", "nodes": []}}})

    def call_async(self, tool, args, sink, run_id="r1"):
        thread = threading.Thread(
            target=lambda: sink.append(self.host.tool_call(run_id, tool, args)))
        thread.daemon = True
        thread.start()
        return thread


class StartTest(HostTestCase):
    def test_starting_a_run_reports_the_run_directory(self):
        self.start()
        started = messages_of(self.out, "run.started")
        self.assertEqual(len(started), 1)
        self.assertTrue(started[0]["runDir"].endswith("r1"))
        self.assertTrue(os.path.isdir(started[0]["runDir"]))

    def test_the_task_reaches_the_agent_inside_an_untrusted_content_framing(self):
        self.start()
        prompt = self.launcher.calls[0]["prompt"]
        self.assertIn("Task: t", prompt)
        self.assertIn("untrusted", prompt)

    def test_a_duplicate_run_id_is_refused(self):
        self.start()
        self.start()
        self.assertEqual([e["code"] for e in messages_of(self.out, "error")],
                         ["duplicate_run"])

    def test_a_launch_failure_is_reported_and_closes_the_socket(self):
        out = RecordingOut()
        host = Host(out, base_dir=self.base, ipc_factory=FakeIpcServer,
                    launcher=FakeLauncher(fail_with=RuntimeError("no working claude CLI")))
        thread = host.handle({"type": "run.start", "runId": "r2", "provider": "claude",
                              "task": "t", "target": dict(TARGET)})
        thread.join(timeout=10)
        errors = messages_of(out, "error")
        self.assertEqual(errors[0]["code"], "start_failed")
        self.assertIn("no working claude CLI", errors[0]["message"])
        self.assertTrue(FakeIpcServer.instances[-1].closed)
        self.assertEqual(host.run_state("r2"), "revoked")
        self.assertFalse(os.path.exists(os.path.join(self.base, "r2", "agent-token")))

    def test_stopping_during_a_slow_launch_kills_the_process_that_arrives_late(self):
        gate = threading.Event()
        launcher = FakeLauncher()
        slow = lambda *a, **k: (gate.wait(10), launcher(*a, **k))[1]
        host = Host(RecordingOut(), base_dir=self.base, ipc_factory=FakeIpcServer,
                    launcher=slow)
        thread = host.handle({"type": "run.start", "runId": "r3", "provider": "claude",
                              "task": "t", "target": dict(TARGET)})
        host.handle({"type": "run.stop", "runId": "r3"})
        self.assertEqual(host.run_state("r3"), "stopped")
        gate.set()
        thread.join(timeout=10)
        self.assertTrue(launcher.last.terminated)
        self.assertEqual(host.tool_call("r3", "browser_snapshot", {})["error"],
                         "run_not_active")

    def test_a_run_is_stoppable_before_its_cli_has_started(self):
        gate = threading.Event()
        launcher = FakeLauncher()
        host = Host(RecordingOut(), base_dir=self.base, ipc_factory=FakeIpcServer,
                    launcher=lambda *a, **k: (gate.wait(10), launcher(*a, **k))[1])
        thread = host.handle({"type": "run.start", "runId": "r4", "provider": "claude",
                              "task": "t", "target": dict(TARGET)})
        self.assertEqual(host.run_state("r4"), "active")
        host.handle({"type": "target.revoked", "runId": "r4", "reason": "tab_closed"})
        self.assertEqual(host.run_state("r4"), "revoked")
        gate.set()
        thread.join(timeout=10)


class MalformedInputTest(HostTestCase):
    def test_a_non_object_frame_does_not_crash_the_host(self):
        for raw in (["run.start"], "run.start", 7, None):
            self.host.handle(raw)
        self.assertEqual([e["code"] for e in messages_of(self.out, "error")],
                         ["bad_message"] * 4)

    def test_a_non_string_run_id_is_reported_without_echoing_a_structure(self):
        self.host.handle({"type": "run.stop", "runId": {"$ne": None}})
        self.assertIsNone(messages_of(self.out, "error")[0]["runId"])


class ToolCallTest(HostTestCase):
    def test_a_tool_call_is_relayed_with_the_target_tab(self):
        self.start()
        results = []
        self.call_async("browser_snapshot", {}, results)
        self.assertTrue(self.host.wait_for_pending(1))
        call = messages_of(self.out, "tool.call")[0]
        self.assertEqual(call["tool"], "browser_snapshot")
        self.assertEqual(call["tabId"], 5)
        self.snapshot(1, call["callId"])
        self.assertTrue(self.host.wait_for_pending(0, timeout=2))

    def test_a_tool_outside_the_allowlist_never_reaches_the_extension(self):
        self.start()
        result = self.host.tool_call("r1", "browser_eval", {"code": "1"})
        self.assertEqual(result["error"], "tool_not_allowed")
        self.assertEqual(messages_of(self.out, "tool.call"), [])

    def test_a_call_for_an_unknown_run_is_refused(self):
        self.assertEqual(self.host.tool_call("nope", "browser_snapshot", {})["error"],
                         "unknown_run")

    def test_a_result_whose_origin_drifted_fails_the_call_and_revokes_the_run(self):
        self.start()
        results = []
        self.call_async("browser_snapshot", {}, results)
        self.assertTrue(self.host.wait_for_pending(1))
        call_id = messages_of(self.out, "tool.call")[0]["callId"]
        self.snapshot(1, call_id, url="https://evil.test/page")
        self.assertTrue(self.host.wait_for_pending(0, timeout=2))
        self.assertEqual(results[0]["error"], "wrong_origin")
        self.assertEqual(self.host.run_state("r1"), "revoked")

    def test_a_replayed_epoch_is_refused(self):
        self.start()
        for epoch, expected in ((1, None), (1, "bad_epoch")):
            results = []
            self.call_async("browser_snapshot", {}, results)
            self.assertTrue(self.host.wait_for_pending(1))
            call_id = messages_of(self.out, "tool.call")[-1]["callId"]
            self.snapshot(epoch, call_id)
            self.assertTrue(self.host.wait_for_pending(0, timeout=2))
            self.assertEqual(results[0].get("error"), expected)

    def test_an_unanswered_call_times_out_rather_than_hanging(self):
        self.start()
        self.assertEqual(self.host.tool_call("r1", "browser_snapshot", {})["error"],
                         "timeout")
        self.assertEqual(self.host.pending_count(), 0)

    def test_a_late_result_for_a_forgotten_call_is_ignored(self):
        self.start()
        self.host.handle({"type": "tool.result", "runId": "r1", "callId": "c999",
                          "result": {}})


class StaleRefTest(HostTestCase):
    def _fresh_snapshot(self, epoch):
        results = []
        self.call_async("browser_snapshot", {}, results)
        self.assertTrue(self.host.wait_for_pending(1))
        self.snapshot(epoch, messages_of(self.out, "tool.call")[-1]["callId"])
        self.assertTrue(self.host.wait_for_pending(0, timeout=2))

    def test_clicking_needs_a_ref_from_the_current_snapshot(self):
        self.start()
        self._fresh_snapshot(1)
        self.assertEqual(
            self.host.tool_call("r1", "browser_click", {"ref": make_ref(0, 2)})["error"],
            "stale_snapshot")

    def test_same_origin_navigation_kills_refs_from_the_old_document(self):
        self.start()
        self._fresh_snapshot(1)
        self.host.handle({"type": "target.navigated", "runId": "r1",
                          "url": "https://app.test/other"})
        self.assertEqual(
            self.host.tool_call("r1", "browser_click", {"ref": make_ref(1, 2)})["error"],
            "stale_snapshot")
        self.assertEqual(self.host.run_state("r1"), "active")

    def test_cross_origin_navigation_ends_the_run(self):
        self.start()
        self.host.handle({"type": "target.navigated", "runId": "r1",
                          "url": "https://evil.test/"})
        self.assertEqual(self.host.run_state("r1"), "revoked")
        self.assertTrue(self.launcher.last.terminated)
        self.assertEqual(
            self.host.tool_call("r1", "browser_snapshot", {})["error"], "run_not_active")


class StopTest(HostTestCase):
    def test_stop_kills_the_agent_process_group(self):
        self.start()
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.assertTrue(self.launcher.last.terminated)

    def test_stop_rejects_every_call_already_in_flight(self):
        self.start()
        first, second = [], []
        self.call_async("browser_snapshot", {}, first)
        self.assertTrue(self.host.wait_for_pending(1))
        self.call_async("browser_screenshot", {}, second)
        self.assertTrue(self.host.wait_for_pending(2))
        self.host.handle({"type": "run.stop", "runId": "r1"})
        for sink in (first, second):
            for _ in range(50):
                if sink:
                    break
                threading.Event().wait(0.05)
            self.assertEqual(sink[0]["error"], "run_stopped")
        self.assertEqual(self.host.pending_count(), 0)

    def test_a_call_arriving_after_stop_is_refused(self):
        self.start()
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.assertEqual(self.host.tool_call("r1", "browser_snapshot", {})["error"],
                         "run_not_active")

    def test_a_straggler_from_the_mcp_socket_is_refused_too(self):
        self.start()
        handler = FakeIpcServer.instances[-1].handler
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.assertEqual(
            handler({"tool": "browser_click", "args": {"ref": "e1:1"}})["error"],
            "run_not_active")

    def test_the_ipc_socket_is_closed_and_the_token_destroyed(self):
        self.start()
        token_file = os.path.join(self.base, "r1", "agent-token")
        open(token_file, "w").close()
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.assertTrue(FakeIpcServer.instances[-1].closed)
        self.assertFalse(os.path.exists(token_file))

    def test_the_run_outputs_survive_the_stop(self):
        self.start()
        self.host.handle({"type": "run.stop", "runId": "r1"})
        root = os.path.join(self.base, "r1")
        self.assertTrue(os.path.isdir(root))
        self.assertTrue(os.path.isdir(os.path.join(root, "files")))
        self.assertTrue(os.path.exists(os.path.join(root, "run.json")))
        self.assertTrue(os.path.exists(os.path.join(root, "transcript.jsonl")))

    def test_stop_tells_the_extension_to_detach(self):
        self.start()
        self.host.handle({"type": "run.stop", "runId": "r1"})
        ended = messages_of(self.out, "run.ended")
        self.assertEqual(len(ended), 1)
        self.assertEqual(ended[0]["state"], "stopped")
        self.assertEqual(ended[0]["reason"], "user")

    def test_stopping_twice_ends_the_run_once(self):
        self.start()
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.assertEqual(len(messages_of(self.out, "run.ended")), 1)
        self.assertEqual(self.launcher.last.terminate_calls, 1)

    def test_a_late_revoke_does_not_reclassify_a_stopped_run(self):
        self.start()
        self.host.handle({"type": "run.stop", "runId": "r1"})
        self.host.handle({"type": "target.revoked", "runId": "r1", "reason": "tab_closed"})
        self.assertEqual(self.host.run_state("r1"), "stopped")

    def test_shutdown_ends_every_live_run(self):
        self.start("r1")
        self.start("r2")
        self.host.shutdown()
        self.assertEqual(self.host.run_state("r1"), "revoked")
        self.assertEqual(self.host.run_state("r2"), "revoked")


class OutputTest(unittest.TestCase):
    def test_recognised_events_become_plain_text(self):
        line = (b'{"type":"assistant","message":{"content":'
                b'[{"type":"text","text":"Clicked Save."}]}}')
        self.assertEqual(Host._plain_text(line), "Clicked Save.")

    def test_codex_style_events_become_plain_text(self):
        self.assertEqual(Host._plain_text(b'{"last_agent_message":"Done."}'), "Done.")

    def test_unrecognised_shapes_produce_nothing_rather_than_a_guess(self):
        for line in (b'not json', b'{"type":"system","subtype":"init"}', b'[]', b'{}'):
            self.assertIsNone(Host._plain_text(line))

    def test_output_is_capped(self):
        line = b'{"text":"' + b'x' * 20000 + b'"}'
        self.assertEqual(len(Host._plain_text(line)), 4000)


if __name__ == "__main__":
    unittest.main()
