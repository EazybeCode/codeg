import json
import os
import shutil
import tempfile
import unittest

from neev_browser.bounds import (MAX_NAME_CHARS, MAX_NODES, MAX_SNAPSHOT_BYTES,
                                 bound_snapshot)
from neev_browser.runs import (RunDirError, append_transcript, create_run,
                               open_private, write_run_json)


def node(i, **kw):
    base = {"ref": "e0:%d" % i, "role": "button", "name": "Item %d" % i}
    base.update(kw)
    return base


def snapshot(nodes, **kw):
    payload = {"url": "https://x.test/", "title": "t", "nodes": nodes}
    payload.update(kw)
    return payload


class BoundsTest(unittest.TestCase):
    def test_node_count_is_capped_and_truncation_is_reported(self):
        out = bound_snapshot(snapshot([node(i) for i in range(MAX_NODES + 50)]))
        self.assertEqual(len(out["nodes"]), MAX_NODES)
        self.assertTrue(out["truncated"])

    def test_a_small_snapshot_reports_no_truncation(self):
        self.assertFalse(bound_snapshot(snapshot([node(0)]))["truncated"])

    def test_names_are_clipped(self):
        out = bound_snapshot(snapshot([node(0, name="z" * 900)]))
        self.assertEqual(len(out["nodes"][0]["name"]), MAX_NAME_CHARS)

    def test_field_values_never_survive_bounding(self):
        out = bound_snapshot(snapshot(
            [node(0, role="textbox", value="hunter2", filled=True)]))
        self.assertNotIn("value", out["nodes"][0])
        self.assertTrue(out["nodes"][0]["filled"])

    def test_password_and_hidden_nodes_keep_only_a_redaction_marker(self):
        out = bound_snapshot(snapshot([
            node(0, role="password", name="Password", value="hunter2"),
            node(1, role="hidden", name="csrf_token", value="abc123"),
        ]))
        blob = json.dumps(out)
        for got in out["nodes"]:
            self.assertEqual(got["role"], "redacted")
            self.assertNotIn("name", got)
        self.assertNotIn("hunter2", blob)
        self.assertNotIn("abc123", blob)
        self.assertNotIn("Password", blob)

    def test_unlisted_top_level_keys_are_dropped_wholesale(self):
        out = bound_snapshot(snapshot([node(0)], cookies="a=b",
                                      localStorage={"k": "v"}, html="<b>hi</b>"))
        for leaked in ("cookies", "localStorage", "html"):
            self.assertNotIn(leaked, out)

    def test_unlisted_node_keys_are_dropped(self):
        out = bound_snapshot(snapshot([node(0, outerHTML="<input value=x>",
                                            autocomplete="cc-number")]))
        self.assertNotIn("outerHTML", out["nodes"][0])
        self.assertNotIn("autocomplete", out["nodes"][0])

    def test_total_byte_budget_is_enforced(self):
        out = bound_snapshot(snapshot(
            [node(i, name="y" * MAX_NAME_CHARS, placeholder="p" * MAX_NAME_CHARS)
             for i in range(MAX_NODES)]))
        self.assertLessEqual(len(json.dumps(out).encode("utf-8")), MAX_SNAPSHOT_BYTES)
        self.assertTrue(out["truncated"])

    def test_a_non_dict_snapshot_degrades_safely(self):
        self.assertEqual(bound_snapshot("<html>")["nodes"], [])


class TruncationSignalTest(unittest.TestCase):
    """The extension clips to MAX_NODES itself, so counting nodes here would
    always report a full read. The flag has to cross the boundary intact."""

    def test_a_flag_set_by_the_extension_survives(self):
        sent = snapshot([node(i) for i in range(MAX_NODES)], truncated=True)
        self.assertTrue(bound_snapshot(sent)["truncated"])

    def test_a_full_page_within_the_caps_still_reports_no_truncation(self):
        sent = snapshot([node(i) for i in range(MAX_NODES)], truncated=False)
        self.assertFalse(bound_snapshot(sent)["truncated"])

    def test_the_host_still_reports_truncation_the_extension_did_not(self):
        sent = snapshot([node(i) for i in range(MAX_NODES + 1)])
        self.assertTrue(bound_snapshot(sent)["truncated"])

    def test_a_non_boolean_flag_is_not_treated_as_truncated(self):
        for value in ("false", "true", 1, 0, [], None, {}):
            sent = snapshot([node(0)], truncated=value)
            self.assertFalse(bound_snapshot(sent)["truncated"], repr(value))

    def test_the_flag_is_never_carried_as_a_node_or_a_string(self):
        out = bound_snapshot(snapshot([node(0)], truncated=True))
        self.assertIs(out["truncated"], True)


class RunsTest(unittest.TestCase):
    def setUp(self):
        self.base = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.base, True)

    def test_run_directory_is_private_and_has_a_files_subdirectory(self):
        paths = create_run("run-1", base_dir=self.base)
        self.assertTrue(os.path.isdir(paths.files))
        self.assertEqual(os.stat(paths.root).st_mode & 0o777, 0o700)

    def test_a_run_id_cannot_escape_the_base_directory(self):
        for bad in ("../../etc", "a/b", "", "x" * 65):
            with self.assertRaises(ValueError):
                create_run(bad, base_dir=self.base)

    def test_transcript_appends_one_json_object_per_line(self):
        paths = create_run("run-1", base_dir=self.base)
        append_transcript(paths, {"kind": "agent", "text": "hi"})
        append_transcript(paths, {"kind": "tool", "name": "browser_snapshot"})
        with open(paths.transcript) as handle:
            lines = [json.loads(line) for line in handle if line.strip()]
        self.assertEqual([entry["kind"] for entry in lines], ["agent", "tool"])

    def test_run_json_is_readable_back(self):
        paths = create_run("run-1", base_dir=self.base)
        write_run_json(paths, {"provider": "claude", "state": "stopped"})
        with open(paths.run_json) as handle:
            self.assertEqual(json.load(handle)["provider"], "claude")

    def test_reusing_a_run_id_is_an_error_not_a_silent_merge(self):
        create_run("run-1", base_dir=self.base)
        with self.assertRaises(RunDirError):
            create_run("run-1", base_dir=self.base)

    def test_a_symlinked_base_directory_is_refused(self):
        elsewhere = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, elsewhere, True)
        link = os.path.join(self.base, "linked-base")
        os.symlink(elsewhere, link)
        with self.assertRaises(RunDirError):
            create_run("run-1", base_dir=link)

    def test_a_world_readable_base_directory_is_refused(self):
        loose = os.path.join(self.base, "loose")
        os.mkdir(loose, 0o755)
        with self.assertRaises(RunDirError):
            create_run("run-1", base_dir=loose)

    def test_run_files_never_follow_a_preplaced_symlink(self):
        paths = create_run("run-1", base_dir=self.base)
        target = os.path.join(self.base, "stolen.txt")
        os.symlink(target, paths.transcript)
        with self.assertRaises(OSError):
            append_transcript(paths, {"kind": "agent"})
        self.assertFalse(os.path.exists(target))

    def test_private_files_are_owner_only(self):
        paths = create_run("run-1", base_dir=self.base)
        with open_private(paths.mcp_config) as handle:
            handle.write("{}")
        self.assertEqual(os.stat(paths.mcp_config).st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
