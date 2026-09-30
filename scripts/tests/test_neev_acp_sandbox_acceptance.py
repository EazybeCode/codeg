"""The ACP sandbox acceptance must never touch a production credential store."""

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "operations"))

import neev_acp_sandbox_acceptance as acceptance  # noqa: E402


class StagingGuardTests(unittest.TestCase):
    def env(self, root):
        return {"NEEV_SUPERVISOR_STORE_ROOT": os.path.join(root, "credentials"),
                "NEEV_SUPERVISOR_REGISTRY": os.path.join(root, "registry.json")}

    def test_a_fresh_or_emptied_staging_store_is_accepted(self):
        with tempfile.TemporaryDirectory() as root:
            env = self.env(root)
            self.assertIsNone(acceptance.staging_refusal(env))       # not created yet
            os.mkdir(env["NEEV_SUPERVISOR_STORE_ROOT"])
            open(os.path.join(env["NEEV_SUPERVISOR_STORE_ROOT"], ".lock"), "w").close()
            self.assertIsNone(acceptance.staging_refusal(env))       # only the lock

    def test_a_store_with_any_volume_is_refused(self):
        with tempfile.TemporaryDirectory() as root:
            env = self.env(root)
            os.makedirs(os.path.join(env["NEEV_SUPERVISOR_STORE_ROOT"], "a" * 32))
            self.assertIn("not empty", acceptance.staging_refusal(env))

    def test_a_supervisor_registry_is_refused(self):
        with tempfile.TemporaryDirectory() as root:
            env = self.env(root)
            open(env["NEEV_SUPERVISOR_REGISTRY"], "w").close()
            self.assertIn("registry", acceptance.staging_refusal(env))

    def test_unconfigured_paths_are_refused(self):
        self.assertIsNotNone(acceptance.staging_refusal({}))
        self.assertIsNotNone(acceptance.staging_refusal(
            {"NEEV_SUPERVISOR_STORE_ROOT": "relative", "NEEV_SUPERVISOR_REGISTRY": "/r"}))

    def test_without_opt_in_it_exits_unmet_before_anything(self):
        saved = os.environ.pop("NEEV_ACP_SANDBOX_ACCEPTANCE", None)
        try:
            self.assertEqual(acceptance.main([]), 2)
        finally:
            if saved is not None:
                os.environ["NEEV_ACP_SANDBOX_ACCEPTANCE"] = saved


if __name__ == "__main__":
    unittest.main()
