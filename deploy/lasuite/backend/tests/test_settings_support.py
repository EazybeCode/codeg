"""Wiring rules applied to upstream's real v5.7.0 lists.

The lists below are copied from pinned upstream source. If an upstream bump moves
`AuthenticationMiddleware`, renames `ForceSessionMiddleware` or drops the OIDC
backend, these tests fail before a container is built.
"""

from django.test import SimpleTestCase

from neev_bridge.settings_support import (
    AUTH_MIDDLEWARE,
    CSRF_MIDDLEWARE,
    MODEL_BACKEND,
    NEEV_FORCE_SESSION,
    OIDC_BACKEND,
    REMOTE_USER_BACKEND,
    REMOTE_USER_MIDDLEWARE,
    THREAD_SCOPE_MIDDLEWARE,
    UPSTREAM_FORCE_SESSION,
    build_authentication_backends,
    build_middleware,
    build_redirect_exempt,
    check_wiring,
)

# impress/settings.py, Base.MIDDLEWARE at v5.7.0.
UPSTREAM_MIDDLEWARE = [
    "django.middleware.security.SecurityMiddleware",
    "dockerflow.django.middleware.DockerflowMiddleware",
    "servestatic.middleware.ServeStaticMiddleware",
    "django.contrib.sessions.middleware.SessionMiddleware",
    "django.middleware.locale.LocaleMiddleware",
    "django.middleware.clickjacking.XFrameOptionsMiddleware",
    "corsheaders.middleware.CorsMiddleware",
    "django.middleware.common.CommonMiddleware",
    "django.middleware.csrf.CsrfViewMiddleware",
    "django.contrib.auth.middleware.AuthenticationMiddleware",
    "core.middleware.ForceSessionMiddleware",
    "core.middleware.SaveRawBodyMiddleware",
    "django.contrib.messages.middleware.MessageMiddleware",
    "csp.middleware.CSPMiddleware",
    "waffle.middleware.WaffleMiddleware",
]

# impress/settings.py, Base.AUTHENTICATION_BACKENDS at v5.7.0.
UPSTREAM_BACKENDS = [MODEL_BACKEND, OIDC_BACKEND]


class MiddlewareWiringTests(SimpleTestCase):
    """Position matters; upstream's own entries must survive."""

    def setUp(self):
        self.wired = build_middleware(UPSTREAM_MIDDLEWARE)

    def test_remote_user_runs_after_authentication_middleware(self):
        self.assertGreater(
            self.wired.index(REMOTE_USER_MIDDLEWARE),
            self.wired.index(AUTH_MIDDLEWARE),
        )

    def test_remote_user_runs_before_the_forced_session(self):
        self.assertLess(
            self.wired.index(REMOTE_USER_MIDDLEWARE),
            self.wired.index(NEEV_FORCE_SESSION),
        )

    def test_csrf_middleware_is_untouched(self):
        self.assertEqual(
            self.wired.index(CSRF_MIDDLEWARE),
            UPSTREAM_MIDDLEWARE.index(CSRF_MIDDLEWARE),
        )

    def test_force_session_is_replaced_not_removed(self):
        self.assertNotIn(UPSTREAM_FORCE_SESSION, self.wired)
        # Two bridge entries are inserted above it, so it shifts by exactly two.
        self.assertEqual(
            self.wired.index(NEEV_FORCE_SESSION),
            UPSTREAM_MIDDLEWARE.index(UPSTREAM_FORCE_SESSION) + 2,
        )

    def test_the_thread_scope_guard_runs_after_identity_and_before_the_view(self):
        self.assertGreater(
            self.wired.index(THREAD_SCOPE_MIDDLEWARE),
            self.wired.index(REMOTE_USER_MIDDLEWARE),
        )
        self.assertLess(
            self.wired.index(THREAD_SCOPE_MIDDLEWARE),
            self.wired.index(NEEV_FORCE_SESSION),
        )

    def test_every_other_upstream_entry_is_preserved_in_order(self):
        inserted = {REMOTE_USER_MIDDLEWARE, THREAD_SCOPE_MIDDLEWARE}
        kept = [m for m in self.wired if m not in inserted]
        expected = [
            NEEV_FORCE_SESSION if m == UPSTREAM_FORCE_SESSION else m
            for m in UPSTREAM_MIDDLEWARE
        ]
        self.assertEqual(kept, expected)

    def test_applying_twice_is_idempotent(self):
        self.assertEqual(build_middleware(self.wired), self.wired)

    def test_missing_anchor_is_an_error_not_a_silent_append(self):
        with self.assertRaises(ValueError):
            build_middleware([m for m in UPSTREAM_MIDDLEWARE if m != AUTH_MIDDLEWARE])


class BackendWiringTests(SimpleTestCase):
    """The bridge backend resolves first; OIDC is gone."""

    def setUp(self):
        self.backends = build_authentication_backends(UPSTREAM_BACKENDS)

    def test_bridge_backend_is_first(self):
        self.assertEqual(self.backends[0], REMOTE_USER_BACKEND)

    def test_model_backend_is_kept(self):
        self.assertIn(MODEL_BACKEND, self.backends)

    def test_oidc_backend_is_removed(self):
        self.assertNotIn(OIDC_BACKEND, self.backends)


class WiringCheckTests(SimpleTestCase):
    """`check_wiring` is what makes a mistake a startup failure."""

    def test_correct_wiring_reports_nothing(self):
        self.assertEqual(
            check_wiring(
                build_middleware(UPSTREAM_MIDDLEWARE),
                build_authentication_backends(UPSTREAM_BACKENDS),
            ),
            [],
        )

    def test_detects_a_missing_thread_scope_guard(self):
        broken = [m for m in build_middleware(UPSTREAM_MIDDLEWARE)
                  if m != THREAD_SCOPE_MIDDLEWARE]
        self.assertIn(
            "the thread-scope guard is not installed",
            check_wiring(broken, build_authentication_backends(UPSTREAM_BACKENDS)),
        )

    def test_detects_removed_csrf_middleware(self):
        broken = [m for m in build_middleware(UPSTREAM_MIDDLEWARE) if m != CSRF_MIDDLEWARE]
        self.assertIn(
            "CsrfViewMiddleware has been removed",
            check_wiring(broken, build_authentication_backends(UPSTREAM_BACKENDS)),
        )

    def test_detects_a_backend_ordering_mistake(self):
        problems = check_wiring(
            build_middleware(UPSTREAM_MIDDLEWARE), [MODEL_BACKEND, REMOTE_USER_BACKEND]
        )
        self.assertIn(
            "the bridge backend is not first in AUTHENTICATION_BACKENDS", problems
        )

    def test_detects_a_surviving_oidc_backend(self):
        problems = check_wiring(
            build_middleware(UPSTREAM_MIDDLEWARE),
            [REMOTE_USER_BACKEND, MODEL_BACKEND, OIDC_BACKEND],
        )
        self.assertIn("the OIDC backend is still enabled", problems)

    def test_detects_middleware_that_runs_too_early(self):
        broken = [REMOTE_USER_MIDDLEWARE] + UPSTREAM_MIDDLEWARE
        problems = check_wiring(broken, build_authentication_backends(UPSTREAM_BACKENDS))
        self.assertIn(
            "remote-user middleware runs before AuthenticationMiddleware", problems
        )


# impress/settings.py, Production.SECURE_REDIRECT_EXEMPT at v5.7.0.
UPSTREAM_REDIRECT_EXEMPT = ["^__lbheartbeat__", "^__heartbeat__"]

DOC = "4c0c8293-f5ed-45aa-b0cb-bceab9038be1"


class CollaborationRedirectTests(SimpleTestCase):
    """y-provider's callbacks over plain HTTP, through Django's real redirect.

    Production redirected them to `https://backend:8000`, which has no TLS, and
    every collaboration socket was refused. Driven through `SecurityMiddleware`
    itself rather than by matching the patterns by hand, so what is asserted is
    the redirect Django actually performs.
    """

    def status(self, path, forwarded_https=False):
        from django.http import HttpResponse
        from django.middleware.security import SecurityMiddleware
        from django.test import RequestFactory, override_settings

        headers = {"HTTP_X_FORWARDED_PROTO": "https"} if forwarded_https else {}
        request = RequestFactory().get(path, **headers)
        with override_settings(
            SECURE_SSL_REDIRECT=True,
            SECURE_PROXY_SSL_HEADER=("HTTP_X_FORWARDED_PROTO", "https"),
            SECURE_REDIRECT_EXEMPT=build_redirect_exempt(UPSTREAM_REDIRECT_EXEMPT),
        ):
            middleware = SecurityMiddleware(lambda _: HttpResponse("ok"))
            return middleware(request).status_code

    def test_the_socket_authorisation_reads_are_answered_not_redirected(self):
        self.assertEqual(self.status(f"/api/v1.0/documents/{DOC}/"), 200)
        self.assertEqual(self.status("/api/v1.0/users/me/"), 200)

    def test_everything_else_over_plain_http_is_still_redirected(self):
        for path in (
            f"/api/v1.0/documents/{DOC}/content/",
            f"/api/v1.0/documents/{DOC}/threads/",
            "/api/v1.0/documents/",
            "/api/v1.0/documents/not-a-uuid/",
            "/api/v1.0/users/me/extra/",
            "/_neev/v1/documents/provision",
        ):
            with self.subTest(path=path):
                self.assertEqual(self.status(path), 301)

    def test_proxied_requests_are_unaffected(self):
        self.assertEqual(self.status(f"/api/v1.0/documents/{DOC}/content/", True), 200)

    def test_upstream_exemptions_are_kept_and_not_duplicated(self):
        once = build_redirect_exempt(UPSTREAM_REDIRECT_EXEMPT)
        self.assertEqual(once[:2], UPSTREAM_REDIRECT_EXEMPT)
        self.assertEqual(build_redirect_exempt(once), once)
