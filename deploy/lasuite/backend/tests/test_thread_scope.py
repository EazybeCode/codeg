"""Which paths the thread-scope guard inspects, and which it leaves alone.

The database half — that a mismatched pair is refused — is in the in-image
suite, where there is a real Thread model to mismatch.
"""

from django.test import SimpleTestCase

from neev_bridge.thread_scope import scoped_pair

DOC = "b3b1f0d2-8c4a-4f5e-9a1b-2c3d4e5f6a7b"
THREAD = "11111111-2222-4333-8444-555555555555"


class ScopedPairTests(SimpleTestCase):
    """Only nested thread routes carry a pairing to check."""

    def test_nested_thread_routes_are_inspected(self):
        for path in (
            f"/api/v1.0/documents/{DOC}/threads/{THREAD}/",
            f"/api/v1.0/documents/{DOC}/threads/{THREAD}",
            f"/api/v1.0/documents/{DOC}/threads/{THREAD}/comments/",
            f"/api/v1.0/documents/{DOC}/threads/{THREAD}/comments/{THREAD}/",
            f"/api/v1.0/documents/{DOC}/threads/{THREAD}/comments/{THREAD}/reactions/",
        ):
            with self.subTest(path=path):
                self.assertEqual(scoped_pair(path), (DOC, THREAD))

    def test_routes_without_a_thread_are_left_alone(self):
        """A thread list has no pairing yet; upstream scopes it by itself."""
        for path in (
            f"/api/v1.0/documents/{DOC}/threads/",
            f"/api/v1.0/documents/{DOC}/",
            f"/api/v1.0/documents/{DOC}/content/",
            "/api/v1.0/users/me/",
            "/_neev/v1/health",
            "/media/x/attachments/y.png",
            "",
            None,
        ):
            with self.subTest(path=path):
                self.assertIsNone(scoped_pair(path))

    def test_a_similar_prefix_is_not_a_thread_route(self):
        self.assertIsNone(scoped_pair(f"/api/v1.0/documents/{DOC}/threadsomething/x/"))

    def test_other_api_versions_are_still_inspected(self):
        """The guard must not fall off when the API version moves."""
        self.assertEqual(
            scoped_pair(f"/api/v2.0/documents/{DOC}/threads/{THREAD}/comments/"),
            (DOC, THREAD),
        )
