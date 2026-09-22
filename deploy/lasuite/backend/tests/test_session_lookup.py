"""Session-to-subject resolution — the websocket upgrade's identity check.

The attack this closes: a client authenticated natively as A presents a Docs
`docs_sessionid` belonging to B. On HTTP that is harmless, because the proxy
asserts A's identity on every request and the middleware repairs the session. On
the websocket there is no such step — y-provider authorises from the cookie
alone. So the proxy must ask who the cookie belongs to *before* upgrading, and
refuse on anything other than an exact match with the natively derived subject.

These tests assert the mismatch is detectable; refusing on it is the proxy's
side of the contract.
"""

from django.contrib.auth import get_user_model
from django.test import Client, TestCase

from neev_bridge.session_lookup import (
    MAX_MINT_TTL_SECONDS,
    mint_session,
    resolve_session,
)

ACTOR_A = "neev:7:1"
ACTOR_B_SAME_ORG = "neev:7:2"
ACTOR_B_OTHER_ORG = "neev:8:2"


def establish(subject):
    """Authenticate a client as `subject` and return its session cookie value."""
    client = Client()
    client.get("/whoami", HTTP_X_NEEV_REMOTE_USER=subject)
    return client.cookies["docs_sessionid"].value


class ResolveSessionTests(TestCase):
    """A cookie resolves to exactly one subject, or to nothing."""

    def test_resolves_the_subject_that_established_it(self):
        self.assertEqual(resolve_session(establish(ACTOR_A)), ACTOR_A)

    def test_unknown_or_malformed_keys_resolve_to_nothing(self):
        for key in (
            None,
            "",
            "   ",
            "short",
            "x" * 65,
            "not a session key",
            "../../etc/passwd",
            "0123456789abcdef0123456789abcdef",  # well-formed, never issued
            42,
        ):
            with self.subTest(key=key):
                self.assertIsNone(resolve_session(key))

    def test_deleted_session_resolves_to_nothing(self):
        key = establish(ACTOR_A)
        self.assertEqual(resolve_session(key), ACTOR_A)

        from django.contrib.sessions.models import Session

        Session.objects.filter(session_key=key).delete()
        self.assertIsNone(resolve_session(key))

    def test_deactivated_user_resolves_to_nothing(self):
        key = establish(ACTOR_A)
        get_user_model().objects.filter(sub=ACTOR_A).update(is_active=False)
        self.assertIsNone(resolve_session(key))

    def test_session_from_another_backend_is_not_accepted(self):
        """A session Docs minted some other way is not a Neev identity."""
        user = get_user_model()(sub=ACTOR_A, admin_email="a@example.test")
        user.set_unusable_password()
        user.save()
        client = Client()
        client.force_login(
            user, backend="django.contrib.auth.backends.ModelBackend"
        )
        key = client.cookies["docs_sessionid"].value
        self.assertIsNone(resolve_session(key))


class MintSessionTests(TestCase):
    """A session derived server-side, which the client never supplied.

    This is what the proxy forwards on a websocket upgrade instead of the
    browser's cookie, so the socket's identity cannot be chosen by the client.
    """

    def _user(self, subject=ACTOR_A):
        establish(subject)  # goes through the normal backend, as a request would
        return get_user_model().objects.get(sub=subject)

    def test_minted_session_names_the_requested_subject(self):
        key, _ttl = mint_session(self._user())
        self.assertEqual(resolve_session(key), ACTOR_A)

    def test_minted_session_is_distinct_from_any_browser_session(self):
        browser_cookie = establish(ACTOR_A)
        key, _ttl = mint_session(get_user_model().objects.get(sub=ACTOR_A))
        self.assertNotEqual(key, browser_cookie)

    def test_minted_session_authenticates_a_cookie_only_request(self):
        """Exactly the shape of a y-provider callback: cookie, no header."""
        key, _ttl = mint_session(self._user())

        client = Client()
        client.cookies["docs_sessionid"] = key
        body = client.get("/whoami").json()

        self.assertTrue(body["authenticated"])
        self.assertEqual(body["sub"], ACTOR_A)
        self.assertEqual(body["session_key"], key)

    def test_minting_for_one_actor_never_yields_another(self):
        self._user(ACTOR_A)
        self._user(ACTOR_B_SAME_ORG)
        key, _ttl = mint_session(get_user_model().objects.get(sub=ACTOR_B_SAME_ORG))
        self.assertEqual(resolve_session(key), ACTOR_B_SAME_ORG)

    def test_ttl_is_bounded_at_both_ends(self):
        user = self._user()
        self.assertEqual(mint_session(user, 1)[1], 60)
        self.assertEqual(mint_session(user, 10**9)[1], MAX_MINT_TTL_SECONDS)
        self.assertEqual(mint_session(user, 900)[1], 900)

    def test_expired_minted_session_stops_resolving(self):
        key, _ttl = mint_session(self._user(), 60)
        from django.contrib.sessions.models import Session
        from django.utils import timezone

        Session.objects.filter(session_key=key).update(
            expire_date=timezone.now() - timezone.timedelta(seconds=1)
        )
        self.assertIsNone(resolve_session(key))


class CrossActorCookieTests(TestCase):
    """Native actor A with actor B's Docs cookie is detectable as a mismatch."""

    def test_same_organization_cookie_swap_is_a_mismatch(self):
        cookie_of_b = establish(ACTOR_B_SAME_ORG)
        self.assertEqual(resolve_session(cookie_of_b), ACTOR_B_SAME_ORG)
        self.assertNotEqual(resolve_session(cookie_of_b), ACTOR_A)

    def test_cross_organization_cookie_swap_is_a_mismatch(self):
        cookie_of_b = establish(ACTOR_B_OTHER_ORG)
        self.assertNotEqual(resolve_session(cookie_of_b), ACTOR_A)

    def test_same_account_in_another_organization_is_a_mismatch(self):
        """Org switch: the same human, a different subject, a different session."""
        cookie_in_org_7 = establish("neev:7:42")
        cookie_in_org_8 = establish("neev:8:42")
        self.assertEqual(resolve_session(cookie_in_org_7), "neev:7:42")
        self.assertEqual(resolve_session(cookie_in_org_8), "neev:8:42")
        self.assertNotEqual(cookie_in_org_7, cookie_in_org_8)

    def test_stale_cookie_after_an_organization_switch_no_longer_resolves(self):
        """The HTTP repair path invalidates the old session; the cookie dies with it."""
        client = Client()
        client.get("/whoami", HTTP_X_NEEV_REMOTE_USER="neev:7:42")
        stale = client.cookies["docs_sessionid"].value

        client.get("/whoami", HTTP_X_NEEV_REMOTE_USER="neev:8:42")
        fresh = client.cookies["docs_sessionid"].value

        self.assertNotEqual(stale, fresh)
        self.assertIsNone(
            resolve_session(stale),
            "the pre-switch session must not still name the pre-switch subject",
        )
        self.assertEqual(resolve_session(fresh), "neev:8:42")
