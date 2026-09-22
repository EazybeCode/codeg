"""Session behaviour of the remote-user middleware.

The property these tests defend is narrow and easy to lose: **the session key and
CSRF token must not change while the same principal keeps making requests.** If
they do, every parallel request races, and the collaboration socket — which
y-provider authorises from `docs_sessionid` — connects with a cookie the backend
has already replaced.
"""

from django.contrib.auth import get_user_model
from django.test import Client, TestCase, override_settings

SUBJECT = "neev:7:42"
OTHER_ORG_SUBJECT = "neev:8:42"

STOCK_MIDDLEWARE = [
    "django.contrib.sessions.middleware.SessionMiddleware",
    "django.middleware.csrf.CsrfViewMiddleware",
    "django.contrib.auth.middleware.AuthenticationMiddleware",
    # Upstream Django's own persistent middleware, pointed at the same header.
    "tests.test_middleware.StockPersistentMiddleware",
]


class StockPersistentMiddleware(
    __import__(
        "django.contrib.auth.middleware", fromlist=["PersistentRemoteUserMiddleware"]
    ).PersistentRemoteUserMiddleware
):
    """Django's shipped middleware, reading the Neev header.

    Used only to demonstrate that the problem being fixed is real.
    """

    header = "HTTP_X_NEEV_REMOTE_USER"


def make_user(subject, **fields):
    """Create a user the way the real model requires: never a blank password."""
    user = get_user_model()(sub=subject, **fields)
    user.set_unusable_password()
    user.save()
    return user


def get(client, subject=None, **headers):
    """Issue a probe request, optionally asserting an identity."""
    if subject is not None:
        headers["HTTP_X_NEEV_REMOTE_USER"] = subject
    return client.get("/whoami", **headers)


class SessionStabilityTests(TestCase):
    """Repeated requests by one principal keep one session."""

    def test_first_request_authenticates_and_creates_the_user(self):
        response = get(Client(), SUBJECT)
        body = response.json()
        self.assertTrue(body["authenticated"])
        self.assertEqual(body["sub"], SUBJECT)
        self.assertEqual(get_user_model().objects.filter(sub=SUBJECT).count(), 1)

    def test_session_key_is_stable_across_repeated_requests(self):
        client = Client()
        keys = {get(client, SUBJECT).json()["session_key"] for _ in range(5)}
        self.assertEqual(len(keys), 1, "the session key changed between requests")
        self.assertIsNotNone(keys.pop())

    def test_no_new_session_cookie_is_issued_after_the_first_request(self):
        client = Client()
        first = get(client, SUBJECT)
        self.assertIn("docs_sessionid", first.cookies)

        later = get(client, SUBJECT)
        self.assertNotIn(
            "docs_sessionid",
            later.cookies,
            "a second Set-Cookie would replace the cookie y-provider is using",
        )

    def test_csrf_token_is_stable_across_repeated_requests(self):
        client = Client(enforce_csrf_checks=True)
        tokens = {get(client, SUBJECT).json()["csrf_cookie"] for _ in range(3)}
        self.assertEqual(len(tokens), 1, "auth.login() rotated the CSRF token")

    def test_one_user_row_for_repeated_first_contact(self):
        """Distinct clients, same subject: still one account."""
        for _ in range(3):
            get(Client(), SUBJECT)
        self.assertEqual(get_user_model().objects.filter(sub=SUBJECT).count(), 1)

    @override_settings(MIDDLEWARE=STOCK_MIDDLEWARE)
    def test_stock_middleware_would_rotate_every_request(self):
        """The guard that makes the tests above non-vacuous.

        Same backend, same header, Django's shipped middleware: because
        `get_username()` returns `admin_email` (NULL here) it never matches the
        subject, so every request logs out and logs back in.
        """
        client = Client()
        keys = [get(client, SUBJECT).json()["session_key"] for _ in range(3)]
        self.assertEqual(
            len(set(keys)), len(keys), "expected the stock middleware to rotate"
        )


class PersistenceTests(TestCase):
    """No header is not a logout — y-provider's callbacks carry none."""

    def test_session_survives_a_request_without_the_identity_header(self):
        client = Client()
        first = get(client, SUBJECT).json()

        callback = get(client).json()  # cookie only, as y-provider forwards it
        self.assertTrue(callback["authenticated"])
        self.assertEqual(callback["sub"], SUBJECT)
        self.assertEqual(callback["session_key"], first["session_key"])

    def test_no_header_and_no_cookie_stays_anonymous(self):
        self.assertFalse(get(Client()).json()["authenticated"])


class IdentitySwitchTests(TestCase):
    """A different principal must not inherit the previous session."""

    def test_organization_switch_rotates_the_session(self):
        client = Client()
        first = get(client, SUBJECT).json()
        second = get(client, OTHER_ORG_SUBJECT).json()

        self.assertEqual(second["sub"], OTHER_ORG_SUBJECT)
        self.assertNotEqual(second["session_key"], first["session_key"])
        self.assertEqual(get_user_model().objects.count(), 2)

    def test_same_user_in_two_organizations_is_two_accounts(self):
        get(Client(), SUBJECT)
        get(Client(), OTHER_ORG_SUBJECT)
        subs = set(get_user_model().objects.values_list("sub", flat=True))
        self.assertEqual(subs, {SUBJECT, OTHER_ORG_SUBJECT})


class RefusalTests(TestCase):
    """Malformed and reserved assertions fail closed."""

    def test_malformed_subject_is_not_authenticated(self):
        body = get(Client(), "not-a-neev-subject").json()
        self.assertFalse(body["authenticated"])
        self.assertEqual(get_user_model().objects.count(), 0)

    def test_malformed_subject_drops_an_established_session(self):
        client = Client()
        get(client, SUBJECT)
        body = get(client, "neev:7").json()
        self.assertFalse(
            body["authenticated"],
            "a broken assertion must not inherit the previous identity",
        )

    def test_service_namespace_is_refused_from_the_header(self):
        body = get(Client(), "neev:service:engine").json()
        self.assertFalse(body["authenticated"])
        self.assertEqual(get_user_model().objects.count(), 0)

    def test_inactive_user_cannot_authenticate(self):
        make_user(SUBJECT, is_active=False)
        self.assertFalse(get(Client(), SUBJECT).json()["authenticated"])


class ProfileTests(TestCase):
    """Email and name are written, and never used to resolve anyone."""

    def test_profile_headers_populate_a_new_user(self):
        body = get(
            Client(),
            SUBJECT,
            HTTP_X_NEEV_REMOTE_EMAIL="person@example.test",
            HTTP_X_NEEV_REMOTE_NAME="A Person",
        ).json()
        self.assertEqual(body["email"], "person@example.test")
        self.assertEqual(body["full_name"], "A Person")

    def test_profile_changes_refresh_without_touching_the_session(self):
        client = Client()
        first = get(
            client, SUBJECT, HTTP_X_NEEV_REMOTE_EMAIL="old@example.test"
        ).json()
        second = get(
            client, SUBJECT, HTTP_X_NEEV_REMOTE_EMAIL="new@example.test"
        ).json()

        self.assertEqual(second["email"], "new@example.test")
        self.assertEqual(second["session_key"], first["session_key"])

    def test_email_never_resolves_an_account(self):
        """Same email, two subjects: two accounts, never a merge."""
        get(Client(), SUBJECT, HTTP_X_NEEV_REMOTE_EMAIL="shared@example.test")
        get(
            Client(),
            OTHER_ORG_SUBJECT,
            HTTP_X_NEEV_REMOTE_EMAIL="shared@example.test",
        )
        self.assertEqual(get_user_model().objects.count(), 2)

    def test_unusable_profile_values_are_dropped(self):
        body = get(
            Client(),
            SUBJECT,
            HTTP_X_NEEV_REMOTE_EMAIL="   ",
            HTTP_X_NEEV_REMOTE_NAME="   ",
        ).json()
        self.assertIsNone(body["email"])


class DisplayNameTests(TestCase):
    """Upstream reads `email.split("@")` when a name is empty; it must not be.

    `UserSerializer.get_full_name`/`get_short_name` assume every user has an
    email, which is true under OIDC and not under header identity. A user with
    neither name nor email would 500 `/users/me/`, so the bridge always leaves
    both name fields populated.
    """

    def test_a_user_with_no_profile_headers_still_has_display_names(self):
        body = get(Client(), SUBJECT).json()
        self.assertTrue(body["full_name"])
        self.assertTrue(body["short_name"])
        self.assertIsNone(body["email"])

    def test_the_email_local_part_is_preferred_over_the_subject(self):
        body = get(
            Client(), SUBJECT, HTTP_X_NEEV_REMOTE_EMAIL="person@example.test"
        ).json()
        self.assertEqual(body["full_name"], "person")

    def test_a_supplied_name_always_wins(self):
        body = get(
            Client(),
            SUBJECT,
            HTTP_X_NEEV_REMOTE_EMAIL="person@example.test",
            HTTP_X_NEEV_REMOTE_NAME="A Person",
        ).json()
        self.assertEqual(body["full_name"], "A Person")

    def test_a_later_request_without_the_name_header_keeps_the_real_name(self):
        """The fallback fills an empty field; it never overwrites a real one."""
        client = Client()
        get(client, SUBJECT, HTTP_X_NEEV_REMOTE_NAME="A Person")
        body = get(client, SUBJECT).json()
        self.assertEqual(body["full_name"], "A Person")
