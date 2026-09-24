import unittest

from neev_browser.scope import (ALLOWED_TOOLS, Run, ScopeError, Target,
                                make_ref, normalize_origin, parse_ref)


def active_run(epoch=1):
    run = Run("r1", "claude", Target(7, "https://app.test"))
    run.observe_snapshot(epoch)
    return run


class OriginTest(unittest.TestCase):
    def test_normalization_ignores_path_query_fragment_and_case(self):
        self.assertEqual(normalize_origin("HTTPS://App.Test:443/a/b?c=d#e"),
                         "https://app.test")

    def test_non_default_port_is_part_of_the_origin(self):
        self.assertEqual(normalize_origin("http://x.test:8080/"), "http://x.test:8080")

    def test_a_url_without_a_host_is_refused(self):
        with self.assertRaises(ScopeError):
            normalize_origin("about:blank")

    def test_non_http_schemes_are_refused(self):
        for url in ("file:///etc/passwd", "chrome://settings",
                    "chrome-extension://abc/page.html", "javascript:alert(1)",
                    "data:text/html,<b>x</b>", "ftp://x.test/"):
            with self.assertRaises(ScopeError, msg=url):
                normalize_origin(url)

    def test_userinfo_can_never_smuggle_a_foreign_host(self):
        for url in ("https://app.test@evil.test/", "https://app.test:pw@evil.test/",
                    "https://user@app.test/"):
            with self.assertRaises(ScopeError, msg=url):
                normalize_origin(url)

    def test_ipv6_hosts_are_bracketed_consistently(self):
        self.assertEqual(normalize_origin("https://[::1]/x"), "https://[::1]")
        self.assertEqual(normalize_origin("https://[::1]:8443/x"), "https://[::1]:8443")
        self.assertEqual(normalize_origin("HTTPS://[0:0:0:0:0:0:0:1]:443/"),
                         "https://[0:0:0:0:0:0:0:1]")


class ScopeTest(unittest.TestCase):
    def test_matching_target_is_authorized(self):
        active_run(3).authorize("browser_click", 7, "https://app.test/page",
                                {"ref": make_ref(3, 1)})

    def test_a_different_tab_is_denied(self):
        with self.assertRaises(ScopeError) as ctx:
            active_run().authorize("browser_snapshot", 8, "https://app.test/")
        self.assertEqual(ctx.exception.code, "wrong_tab")

    def test_a_different_origin_is_denied(self):
        with self.assertRaises(ScopeError) as ctx:
            active_run().authorize("browser_snapshot", 7, "https://evil.test/")
        self.assertEqual(ctx.exception.code, "wrong_origin")

    def test_a_tool_outside_the_allowlist_is_denied(self):
        with self.assertRaises(ScopeError) as ctx:
            active_run().authorize("browser_eval", 7, "https://app.test/")
        self.assertEqual(ctx.exception.code, "tool_not_allowed")
        self.assertNotIn("browser_eval", ALLOWED_TOOLS)

    def test_stopped_runs_deny_everything_including_reads(self):
        run = active_run()
        run.stop("user")
        with self.assertRaises(ScopeError) as ctx:
            run.authorize("browser_snapshot", 7, "https://app.test/")
        self.assertEqual(ctx.exception.code, "run_not_active")

    def test_revocation_is_permanent_and_idempotent(self):
        run = active_run()
        run.revoke("origin_changed")
        run.revoke("origin_changed")
        self.assertEqual(run.state, "revoked")
        with self.assertRaises(ScopeError):
            run.authorize("browser_snapshot", 7, "https://app.test/")

    def test_a_stopped_run_cannot_be_reclassified_by_a_late_revoke(self):
        run = active_run()
        run.stop("user")
        run.revoke("late")
        self.assertEqual(run.state, "stopped")
        self.assertEqual(run.stop_reason, "user")


class StaleRefTest(unittest.TestCase):
    def test_refs_encode_the_snapshot_they_came_from(self):
        self.assertEqual(parse_ref(make_ref(4, 12)), 4)

    def test_a_ref_from_an_older_snapshot_is_refused(self):
        run = active_run(5)
        with self.assertRaises(ScopeError) as ctx:
            run.authorize("browser_click", 7, "https://app.test/",
                          {"ref": make_ref(4, 1)})
        self.assertEqual(ctx.exception.code, "stale_snapshot")

    def test_a_ref_from_a_newer_snapshot_is_also_refused(self):
        with self.assertRaises(ScopeError) as ctx:
            active_run(5).authorize("browser_type", 7, "https://app.test/",
                                    {"ref": make_ref(9, 1)})
        self.assertEqual(ctx.exception.code, "stale_snapshot")

    def test_a_malformed_ref_is_refused(self):
        for bad in (None, "", "e1", "button", "e:", "ex:1", 17):
            with self.assertRaises(ScopeError):
                active_run().authorize("browser_click", 7, "https://app.test/",
                                       {"ref": bad})

    def test_reading_the_page_does_not_need_a_ref(self):
        active_run(2).authorize("browser_snapshot", 7, "https://app.test/")

    def test_refs_are_dead_until_the_first_snapshot(self):
        run = Run("r1", "claude", Target(7, "https://app.test"))
        with self.assertRaises(ScopeError) as ctx:
            run.authorize("browser_click", 7, "https://app.test/",
                          {"ref": make_ref(0, 1)})
        self.assertEqual(ctx.exception.code, "stale_snapshot")


class DocumentChangeTest(unittest.TestCase):
    def test_same_origin_navigation_kills_outstanding_refs(self):
        run = active_run(3)
        run.authorize("browser_click", 7, "https://app.test/a", {"ref": make_ref(3, 1)})
        run.invalidate_refs("same_origin_navigation")
        with self.assertRaises(ScopeError) as ctx:
            run.authorize("browser_click", 7, "https://app.test/b",
                          {"ref": make_ref(3, 1)})
        self.assertEqual(ctx.exception.code, "stale_snapshot")

    def test_reading_the_new_document_restores_action_capability(self):
        run = active_run(3)
        run.invalidate_refs("same_origin_navigation")
        run.observe_snapshot(4)
        run.authorize("browser_click", 7, "https://app.test/b", {"ref": make_ref(4, 1)})

    def test_a_ref_from_the_previous_document_stays_dead_after_the_new_snapshot(self):
        run = active_run(3)
        run.invalidate_refs("same_origin_navigation")
        run.observe_snapshot(4)
        with self.assertRaises(ScopeError):
            run.authorize("browser_click", 7, "https://app.test/b",
                          {"ref": make_ref(3, 1)})

    def test_snapshot_epochs_must_move_forward(self):
        run = active_run(5)
        for bad in (5, 4, 0, True, "6", None):
            with self.assertRaises(ScopeError, msg=repr(bad)):
                run.observe_snapshot(bad)

    def test_a_replayed_epoch_after_navigation_is_still_refused(self):
        run = active_run(5)
        run.invalidate_refs("same_origin_navigation")
        with self.assertRaises(ScopeError):
            run.observe_snapshot(4)


if __name__ == "__main__":
    unittest.main()
