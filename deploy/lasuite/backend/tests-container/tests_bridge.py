"""Bridge tests that only mean anything inside the Docs image.

These run against the real `lasuite/impress-backend:v5.7.0`: upstream's own
`core.models.User` and `Document`, the real settings module, the real middleware
chain, a real PostgreSQL and a real Redis session store. Django's test runner
creates and destroys its own `test_docs` database, so nothing here writes to the
stack's data.

The local suite (`tests/`) covers the identity and session rules against a
mirrored user model. This file covers what that cannot: provisioning, ACL
projection, the settings module itself, and the endpoints end to end.

Run with `tests-container/run.sh`, from a stack directory.
"""

import json
import threading
import uuid

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import connections
from django.test import Client, SimpleTestCase, TestCase, TransactionTestCase
from django.urls import Resolver404, resolve

from core import choices, models

from neev_bridge import service
from neev_bridge.identity import SERVICE_SUBJECT
from neev_bridge.session_lookup import mint_session, resolve_session
from neev_bridge.settings_support import (
    AUTH_MIDDLEWARE,
    CSRF_MIDDLEWARE,
    OIDC_BACKEND,
    REMOTE_USER_BACKEND,
    REMOTE_USER_MIDDLEWARE,
    UPSTREAM_FORCE_SESSION,
)

OWNER = "neev:7:1"
MEMBER = "neev:7:2"
OUTSIDER = "neev:8:9"

BRIDGE_KEY_HEADER = "HTTP_X_NEEV_BRIDGE_KEY"


def new_id():
    return str(uuid.uuid4())


def make_user(subject, **fields):
    """Create a user as the real model requires — a blank password is refused."""
    user = get_user_model()(sub=subject, **fields)
    user.set_unusable_password()
    user.save()
    return user


def identity(subject, **extra):
    """Headers as the proxy sets them."""
    return {"HTTP_X_NEEV_REMOTE_USER": subject, **extra}


class SettingsModuleTests(SimpleTestCase):
    """The mounted settings module, as Django actually loaded it."""

    def test_remote_user_middleware_is_wired_after_authentication(self):
        self.assertIn(REMOTE_USER_MIDDLEWARE, settings.MIDDLEWARE)
        self.assertGreater(
            settings.MIDDLEWARE.index(REMOTE_USER_MIDDLEWARE),
            settings.MIDDLEWARE.index(AUTH_MIDDLEWARE),
        )

    def test_upstream_csrf_protection_is_still_installed(self):
        self.assertIn(CSRF_MIDDLEWARE, settings.MIDDLEWARE)

    def test_forced_session_middleware_was_replaced(self):
        self.assertNotIn(UPSTREAM_FORCE_SESSION, settings.MIDDLEWARE)

    def test_authentication_backends(self):
        self.assertEqual(settings.AUTHENTICATION_BACKENDS[0], REMOTE_USER_BACKEND)
        self.assertNotIn(OIDC_BACKEND, settings.AUTHENTICATION_BACKENDS)

    def test_session_cookie_keeps_the_name_y_provider_looks_for(self):
        self.assertEqual(settings.SESSION_COOKIE_NAME, "docs_sessionid")

    def test_cookies_are_scoped_to_the_engine_prefix(self):
        self.assertEqual(settings.SESSION_COOKIE_PATH, "/document-engine/")
        self.assertEqual(settings.CSRF_COOKIE_PATH, "/document-engine/")
        self.assertEqual(settings.SESSION_COOKIE_SAMESITE, "Lax")
        self.assertEqual(settings.CSRF_COOKIE_SAMESITE, "Lax")

    def test_media_and_collaboration_urls_are_left_unset(self):
        """Both would double-prefix or host-pin the frontend if set."""
        self.assertIsNone(settings.MEDIA_BASE_URL)
        self.assertIsNone(settings.COLLABORATION_WS_URL)
        self.assertEqual(settings.MEDIA_URL, "/media/")

    def test_optional_surfaces_are_off(self):
        self.assertFalse(settings.USE_SWAGGER)
        self.assertFalse(settings.SILK_ENABLED)
        self.assertFalse(settings.OIDC_RESOURCE_SERVER_ENABLED)

    def test_host_and_origin_lists_parsed_into_real_values(self):
        """django-configurations' ListValue splits on ",", it does not parse JSON.

        A JSON array in the env file is stored verbatim, so `ALLOWED_HOSTS`
        becomes `['["a"', '"b"]']` and `CSRF_TRUSTED_ORIGINS` holds a string no
        Origin header can ever equal — every unsafe browser request would 403.
        Caught by running against the real image; asserted here so the env
        format cannot silently regress.
        """
        for host in settings.ALLOWED_HOSTS:
            self.assertNotRegex(host, r'[\[\]"]', f"unparsed list syntax in {host!r}")
        self.assertTrue(settings.CSRF_TRUSTED_ORIGINS, "no trusted origin configured")
        for origin in settings.CSRF_TRUSTED_ORIGINS:
            self.assertRegex(origin, r"^https?://[^\s\[\]\"]+$", f"bad origin {origin!r}")

    def test_bridge_key_is_configured_and_long_enough(self):
        self.assertTrue(settings.NEEV_BRIDGE_KEY)
        self.assertGreaterEqual(len(settings.NEEV_BRIDGE_KEY), 32)


class UrlConfTests(SimpleTestCase):
    """What the root URLconf exposes, and what it does not."""

    def test_bridge_endpoints_resolve(self):
        for path in (
            "/_neev/v1/health",
            "/_neev/v1/documents/provision",
            "/_neev/v1/sessions/mint",
            "/_neev/v1/sessions/resolve",
        ):
            with self.subTest(path=path):
                self.assertIsNotNone(resolve(path))

    def test_admin_is_not_routed(self):
        with self.assertRaises(Resolver404):
            resolve("/admin/")

    def test_upstream_api_is_still_routed(self):
        self.assertIsNotNone(resolve("/api/v1.0/documents/"))


class BridgeAuthTests(TestCase):
    """The internal endpoints are credentialled, and fail closed."""

    def post(self, path, payload=None, key=None, **extra):
        headers = dict(extra)
        if key is not None:
            headers[BRIDGE_KEY_HEADER] = key
        return self.client.post(
            path,
            data=json.dumps(payload or {}),
            content_type="application/json",
            secure=True,
            **headers,
        )

    def test_no_key_is_refused(self):
        response = self.post("/_neev/v1/documents/provision", {"id": new_id()})
        self.assertEqual(response.status_code, 401)
        self.assertEqual(response.json()["error"], "unauthorized")

    def test_wrong_key_is_refused(self):
        response = self.post(
            "/_neev/v1/documents/provision", {"id": new_id()}, key="x" * 64
        )
        self.assertEqual(response.status_code, 401)

    def test_health_requires_the_key(self):
        self.assertEqual(self.client.get("/_neev/v1/health", secure=True).status_code, 401)
        self.assertEqual(
            self.client.get(
                "/_neev/v1/health",
                secure=True,
                **{BRIDGE_KEY_HEADER: settings.NEEV_BRIDGE_KEY},
            ).status_code,
            200,
        )

    def test_provisioning_does_not_issue_a_session_cookie(self):
        """Server-to-server calls must not mint browser sessions."""
        response = self.post(
            "/_neev/v1/documents/provision",
            {"id": new_id(), "sub": OWNER, "title": "T"},
            key=settings.NEEV_BRIDGE_KEY,
        )
        self.assertEqual(response.status_code, 201)
        self.assertNotIn("docs_sessionid", response.cookies)

    def test_malformed_input_is_a_refusal_not_a_crash(self):
        cases = [
            ({"id": "not-a-uuid", "sub": OWNER}, "invalid_document_id"),
            ({"id": new_id(), "sub": "nope"}, "invalid_subject"),
            ({"id": new_id(), "sub": SERVICE_SUBJECT}, "invalid_subject"),
            ({"id": new_id(), "sub": OWNER, "title": 42}, "invalid_title"),
            ({"id": str(uuid.uuid1()), "sub": OWNER}, "invalid_document_id"),
        ]
        for payload, expected in cases:
            with self.subTest(payload=payload):
                response = self.post(
                    "/_neev/v1/documents/provision",
                    payload,
                    key=settings.NEEV_BRIDGE_KEY,
                )
                self.assertEqual(response.status_code, 400)
                self.assertEqual(response.json()["error"], expected)

    def test_oversized_body_is_refused(self):
        response = self.client.post(
            "/_neev/v1/documents/provision",
            data=json.dumps({"id": new_id(), "sub": OWNER, "title": "x" * 20000}),
            content_type="application/json",
            secure=True,
            **{BRIDGE_KEY_HEADER: settings.NEEV_BRIDGE_KEY},
        )
        self.assertEqual(response.status_code, 413)


class ProvisioningTests(TestCase):
    """Creating the Docs document that carries a Neev resource id."""

    def test_document_takes_the_neev_resource_id(self):
        document_id = new_id()
        document, created = service.provision_document(document_id, "Notes", OWNER)
        self.assertTrue(created)
        self.assertEqual(str(document.id), document_id)
        self.assertEqual(document.title, "Notes")

    def test_creator_is_the_subject_and_roles_are_as_designed(self):
        document, _ = service.provision_document(new_id(), "Notes", OWNER)
        self.assertEqual(document.creator.sub, OWNER)

        roles = {
            access.user.sub: access.role for access in document.accesses.all()
        }
        self.assertEqual(roles[SERVICE_SUBJECT], choices.RoleChoices.OWNER)
        self.assertEqual(roles[OWNER], choices.RoleChoices.EDITOR)
        self.assertNotIn(
            roles[OWNER],
            (choices.RoleChoices.OWNER, choices.RoleChoices.ADMIN),
            "no human may hold a privileged role in Docs",
        )

    def test_link_sharing_is_off(self):
        document, _ = service.provision_document(new_id(), "Notes", OWNER)
        self.assertEqual(document.link_reach, choices.LinkReachChoices.RESTRICTED)

    def test_provisioning_twice_is_idempotent(self):
        document_id = new_id()
        first, created_first = service.provision_document(document_id, "Notes", OWNER)
        second, created_second = service.provision_document(document_id, "Notes", OWNER)

        self.assertTrue(created_first)
        self.assertFalse(created_second)
        self.assertEqual(first.id, second.id)
        self.assertEqual(models.Document.objects.filter(pk=document_id).count(), 1)
        self.assertEqual(second.accesses.count(), 2)

    def test_reprovisioning_repairs_a_missing_access_row(self):
        document_id = new_id()
        document, _ = service.provision_document(document_id, "Notes", OWNER)
        document.accesses.filter(user__sub=OWNER).delete()

        service.provision_document(document_id, "Notes", OWNER)
        self.assertTrue(document.accesses.filter(user__sub=OWNER).exists())

    def test_another_organization_cannot_claim_an_existing_id(self):
        document_id = new_id()
        service.provision_document(document_id, "Notes", OWNER)
        with self.assertRaises(service.BridgeError) as caught:
            service.provision_document(document_id, "Notes", OUTSIDER)
        self.assertEqual(caught.exception.code, "organization_mismatch")

    def test_a_document_the_bridge_did_not_create_is_refused(self):
        """A pre-existing row — for example from another population — is not ours."""
        user = make_user("other:1:1")
        document = models.Document.add_root(title="Foreign", creator=user)
        with self.assertRaises(service.BridgeError) as caught:
            service.provision_document(str(document.id), "Notes", OWNER)
        self.assertEqual(caught.exception.code, "not_an_engine_document")


class AccessProjectionTests(TestCase):
    """Neev's decision, reflected into Docs' own ACL."""

    def setUp(self):
        self.document_id = new_id()
        self.document, _ = service.provision_document(self.document_id, "Notes", OWNER)

    def role_of(self, subject):
        access = models.DocumentAccess.objects.filter(
            document=self.document, user__sub=subject
        ).first()
        return access.role if access else None

    def test_member_gets_the_projected_role(self):
        _access, changed = service.project_access(
            self.document_id, MEMBER, choices.RoleChoices.EDITOR
        )
        self.assertTrue(changed)
        self.assertEqual(self.role_of(MEMBER), choices.RoleChoices.EDITOR)

    def test_projection_is_idempotent(self):
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.EDITOR)
        _access, changed = service.project_access(
            self.document_id, MEMBER, choices.RoleChoices.EDITOR
        )
        self.assertFalse(changed)
        self.assertEqual(
            models.DocumentAccess.objects.filter(
                document=self.document, user__sub=MEMBER
            ).count(),
            1,
        )

    def test_downgrade_to_reader_is_applied(self):
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.EDITOR)
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.READER)
        self.assertEqual(self.role_of(MEMBER), choices.RoleChoices.READER)

    def test_privileged_roles_cannot_be_projected(self):
        for role in (
            choices.RoleChoices.OWNER,
            choices.RoleChoices.ADMIN,
            choices.RoleChoices.COMMENTER,
            "superuser",
            "",
            None,
        ):
            with self.subTest(role=role):
                with self.assertRaises(service.BridgeError) as caught:
                    service.project_access(self.document_id, MEMBER, role)
                self.assertEqual(caught.exception.code, "invalid_role")

    def test_the_service_principal_cannot_be_downgraded(self):
        with self.assertRaises(service.BridgeError) as caught:
            service.project_access(
                self.document_id, SERVICE_SUBJECT, choices.RoleChoices.READER
            )
        self.assertEqual(caught.exception.code, "invalid_subject")
        self.assertEqual(self.role_of(SERVICE_SUBJECT), choices.RoleChoices.OWNER)

    def test_another_organization_cannot_be_granted_access(self):
        with self.assertRaises(service.BridgeError) as caught:
            service.project_access(
                self.document_id, OUTSIDER, choices.RoleChoices.EDITOR
            )
        self.assertEqual(caught.exception.code, "organization_mismatch")
        self.assertIsNone(self.role_of(OUTSIDER))

    def test_unknown_document_is_a_404(self):
        with self.assertRaises(service.BridgeError) as caught:
            service.project_access(new_id(), MEMBER, choices.RoleChoices.EDITOR)
        self.assertEqual(caught.exception.status, 404)

    def test_revoke_removes_the_projection(self):
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.EDITOR)
        self.assertTrue(service.revoke_access(self.document_id, MEMBER))
        self.assertIsNone(self.role_of(MEMBER))

    def test_revoke_is_idempotent(self):
        self.assertFalse(service.revoke_access(self.document_id, MEMBER))

    def test_revoke_never_removes_the_service_principal(self):
        """The service namespace is not addressable through these calls at all."""
        with self.assertRaises(service.BridgeError) as caught:
            service.revoke_access(self.document_id, SERVICE_SUBJECT)
        self.assertEqual(caught.exception.code, "invalid_subject")
        self.assertEqual(self.role_of(SERVICE_SUBJECT), choices.RoleChoices.OWNER)


class AbilitiesTests(TestCase):
    """Upstream's own permission computation, not the bridge's opinion of it.

    y-provider decides whether a socket may write by reading `abilities.update`.
    These assert the projection actually produces that, using upstream's code.
    """

    def setUp(self):
        self.document_id = new_id()
        self.document, _ = service.provision_document(self.document_id, "Notes", OWNER)

    def user(self, subject):
        return get_user_model().objects.get(sub=subject)

    def test_editor_may_update(self):
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.EDITOR)
        abilities = self.document.get_abilities(self.user(MEMBER))
        self.assertTrue(abilities["update"])

    def test_reader_may_not_update(self):
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.READER)
        abilities = self.document.get_abilities(self.user(MEMBER))
        self.assertTrue(abilities["retrieve"])
        self.assertFalse(abilities["update"])

    def test_unprojected_member_may_not_even_read(self):
        user = make_user(MEMBER)
        abilities = self.document.get_abilities(user)
        self.assertFalse(abilities["retrieve"])

    def test_editor_may_not_manage_sharing_or_destroy(self):
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.EDITOR)
        abilities = self.document.get_abilities(self.user(MEMBER))
        self.assertFalse(abilities["accesses_manage"])
        self.assertFalse(abilities["destroy"])
        self.assertFalse(abilities["link_configuration"])


class RemoteIdentityRequestTests(TestCase):
    """The browser path, through upstream's own API."""

    ME = "/api/v1.0/users/me/"

    def get_me(self, client, subject=None, **extra):
        headers = identity(subject, **extra) if subject else extra
        return client.get(self.ME, secure=True, **headers)

    def test_identity_header_authenticates_against_the_real_api(self):
        response = self.get_me(Client(), OWNER)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(get_user_model().objects.filter(sub=OWNER).count(), 1)

    def test_a_user_with_no_email_does_not_break_the_serializer(self):
        """Upstream's UserSerializer does `instance.email.split("@")`.

        Under OIDC an email always exists; under header identity it is optional,
        and this returned 500 until the bridge started guaranteeing a display
        name. Regression test for that, against upstream's own serializer.
        """
        response = self.get_me(Client(), OWNER)  # no email, no name header
        self.assertEqual(response.status_code, 200)
        body = response.json()
        self.assertTrue(body["full_name"])
        self.assertTrue(body["short_name"])

    def test_a_supplied_name_is_what_upstream_returns(self):
        response = self.get_me(
            Client(),
            OWNER,
            HTTP_X_NEEV_REMOTE_EMAIL="person@example.test",
            HTTP_X_NEEV_REMOTE_NAME="A Person",
        )
        self.assertEqual(response.json()["full_name"], "A Person")
        self.assertEqual(response.json()["email"], "person@example.test")

    def test_no_identity_and_no_cookie_is_unauthenticated(self):
        self.assertIn(self.get_me(Client()).status_code, (401, 403))

    def test_session_and_csrf_are_stable_across_requests(self):
        client = Client()
        self.get_me(client, OWNER)
        session_key = client.session.session_key
        csrf = client.cookies.get("csrftoken")

        for _ in range(4):
            self.assertEqual(self.get_me(client, OWNER).status_code, 200)

        self.assertEqual(client.session.session_key, session_key)
        if csrf is not None:
            self.assertEqual(client.cookies.get("csrftoken").value, csrf.value)

    def test_collaboration_callback_shape_keeps_the_session(self):
        """Cookie only, no identity header — exactly what y-provider forwards."""
        client = Client()
        self.get_me(client, OWNER)
        key = client.session.session_key

        response = self.get_me(client)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(client.session.session_key, key)

    def test_organization_switch_rotates_the_session(self):
        client = Client()
        self.get_me(client, "neev:7:42")
        first = client.session.session_key
        self.get_me(client, "neev:8:42")
        self.assertNotEqual(client.session.session_key, first)

    def test_service_principal_cannot_be_asserted_by_a_client(self):
        response = self.get_me(Client(), SERVICE_SUBJECT)
        self.assertIn(response.status_code, (401, 403))

    def test_session_cookie_is_scoped_to_the_engine_prefix(self):
        client = Client()
        self.get_me(client, OWNER)
        cookie = client.cookies["docs_sessionid"]
        self.assertEqual(cookie["path"], "/document-engine/")
        self.assertEqual(cookie["samesite"], "Lax")

    def test_unsafe_requests_still_require_csrf(self):
        """Upstream's CSRF protection is untouched by the bridge."""
        client = Client(enforce_csrf_checks=True)
        self.get_me(client, OWNER)
        response = client.post(
            "/api/v1.0/documents/",
            data=json.dumps({"title": "x"}),
            content_type="application/json",
            secure=True,
            **identity(OWNER),
        )
        self.assertEqual(response.status_code, 403)


class WebsocketIdentityBindingTests(TestCase):
    """A socket's identity is the one Neev derived, not the one presented."""

    def setUp(self):
        for subject in (OWNER, MEMBER):
            Client().get("/api/v1.0/users/me/", secure=True, **identity(subject))

    def user(self, subject):
        return get_user_model().objects.get(sub=subject)

    def test_minted_session_resolves_to_its_subject(self):
        key, _ttl = mint_session(self.user(OWNER))
        self.assertEqual(resolve_session(key), OWNER)

    def test_minted_session_authenticates_a_cookie_only_request(self):
        key, _ttl = mint_session(self.user(OWNER))
        client = Client()
        client.cookies["docs_sessionid"] = key
        response = client.get("/api/v1.0/users/me/", secure=True)
        self.assertEqual(response.status_code, 200)

    def test_another_actors_cookie_never_resolves_to_this_actor(self):
        client = Client()
        client.get("/api/v1.0/users/me/", secure=True, **identity(MEMBER))
        cookie_of_member = client.cookies["docs_sessionid"].value

        self.assertEqual(resolve_session(cookie_of_member), MEMBER)
        self.assertNotEqual(resolve_session(cookie_of_member), OWNER)

    def test_unknown_cookie_resolves_to_nothing(self):
        self.assertIsNone(resolve_session("0" * 32))


class ConcurrentProvisioningTests(TransactionTestCase):
    """Real threads, real PostgreSQL — the race the retry path exists for."""

    def test_parallel_provisioning_of_one_id_converges(self):
        document_id = new_id()
        results, errors = [], []
        barrier = threading.Barrier(4)

        def provision():
            try:
                barrier.wait(timeout=10)
                results.append(service.provision_document(document_id, "Notes", OWNER))
            except Exception as err:  # noqa: BLE001 - recorded, then asserted on
                errors.append(err)
            finally:
                connections.close_all()

        threads = [threading.Thread(target=provision) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=30)

        self.assertEqual(errors, [], "a provisioning race must not surface an error")
        self.assertEqual(len(results), 4)
        self.assertEqual(models.Document.objects.filter(pk=document_id).count(), 1)
        self.assertEqual(sum(1 for _doc, created in results if created), 1)
        self.assertEqual(
            models.DocumentAccess.objects.filter(document_id=document_id).count(), 2
        )

    def test_parallel_first_contact_creates_one_user(self):
        subject = "neev:7:777"
        errors = []
        barrier = threading.Barrier(4)

        def touch():
            try:
                barrier.wait(timeout=10)
                Client().get("/api/v1.0/users/me/", secure=True, **identity(subject))
            except Exception as err:  # noqa: BLE001
                errors.append(err)
            finally:
                connections.close_all()

        threads = [threading.Thread(target=touch) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=30)

        self.assertEqual(errors, [])
        self.assertEqual(get_user_model().objects.filter(sub=subject).count(), 1)
