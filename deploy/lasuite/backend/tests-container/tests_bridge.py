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
import urllib.parse
import uuid

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.uploadedfile import SimpleUploadedFile
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


PNG = bytes.fromhex(
    "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4"
    "890000000a49444154789c6360000002000100ffff03000006000557bfabd400"
    "00000049454e44ae426082"
)
SVG = b'<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script>alert(1)</script></svg>'
HTML = b"<html><body><script>alert(document.domain)</script></body></html>"


class MediaRelayTests(TestCase):
    """Attachments, read through the bridge instead of a signed URL.

    Uploads go through upstream's own `attachment-upload` so the objects under
    test are stored exactly as production stores them — same key shape, same
    libmagic-derived content type, same `-unsafe` marking, same metadata rewrite
    by the malware-detection callback.
    """

    def setUp(self):
        self.document_id = new_id()
        self.document, _ = service.provision_document(self.document_id, "Media", OWNER)
        service.project_access(self.document_id, MEMBER, choices.RoleChoices.READER)
        self.session = Client()
        self.session.get("/api/v1.0/users/me/", secure=True, **identity(OWNER))

    def upload(self, filename, content, declared="application/octet-stream"):
        """Upload through upstream's endpoint and return the stored key."""
        upload = SimpleUploadedFile(filename, content, content_type=declared)
        response = self.session.post(
            f"/api/v1.0/documents/{self.document_id}/attachment-upload/",
            data={"file": upload},
            secure=True,
            HTTP_X_CSRFTOKEN=self.session.cookies["csrftoken"].value,
            HTTP_ORIGIN=f"https://{settings.ALLOWED_HOSTS[0]}",
            **identity(OWNER),
        )
        self.assertEqual(response.status_code, 201, response.content[:200])
        query = urllib.parse.urlparse(response.json()["file"]).query
        return urllib.parse.parse_qs(query)["key"][0]

    def fetch(self, key, subject=OWNER, document_id=None, key_header=True):
        headers = {BRIDGE_KEY_HEADER: settings.NEEV_BRIDGE_KEY} if key_header else {}
        if subject is not None:
            headers["HTTP_X_NEEV_REMOTE_USER"] = subject
        # `key=None` is the omitted-parameter case; the test client cannot encode
        # it, and omitting it is what the endpoint would actually receive.
        query = {} if key is None else {"key": key}
        return self.client.get(
            f"/_neev/v1/documents/{document_id or self.document_id}/media",
            query,
            secure=True,
            **headers,
        )

    @staticmethod
    def body(response):
        return b"".join(response.streaming_content)

    def test_owner_gets_the_bytes(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.body(response), PNG)

    def test_a_raster_image_is_inline(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key)
        self.assertEqual(response["Content-Type"], "image/png")
        self.assertTrue(response["Content-Disposition"].startswith("inline;"))

    def test_an_svg_is_never_inline(self):
        """The whole point: an SVG is an image to a browser and a script host."""
        key = self.upload("payload.svg", SVG, "image/svg+xml")
        response = self.fetch(key)
        self.assertEqual(response.status_code, 200)
        self.assertTrue(
            response["Content-Disposition"].startswith("attachment;"),
            "an SVG was served inline",
        )

    def test_html_is_never_inline(self):
        key = self.upload("payload.html", HTML, "text/html")
        self.assertIn("-unsafe", key, "upstream should mark HTML unsafe")
        response = self.fetch(key)
        self.assertTrue(response["Content-Disposition"].startswith("attachment;"))

    def test_a_disguised_upload_is_still_not_inline(self):
        """Declared image/png, HTML bytes: libmagic decides, and so do we."""
        key = self.upload("disguise.png", HTML, "image/png")
        response = self.fetch(key)
        self.assertTrue(response["Content-Disposition"].startswith("attachment;"))

    def test_every_response_carries_the_neutralising_headers(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key)
        self.assertEqual(response["X-Content-Type-Options"], "nosniff")
        self.assertEqual(
            response["Content-Security-Policy"], "default-src 'none'; sandbox"
        )
        self.assertEqual(response["Cache-Control"], "private, no-store")
        self.assertEqual(response["Content-Length"], str(len(PNG)))

    def test_no_object_store_metadata_reaches_the_client(self):
        """`x-amz-meta-owner` is a Docs user id; none of it is the browser's business."""
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key)
        leaked = [name for name in response.headers if name.lower().startswith("x-amz")]
        self.assertEqual(leaked, [])

    def test_nothing_is_ever_a_redirect(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key)
        self.assertEqual(response.status_code, 200)
        self.assertNotIn("Location", response.headers)

    def test_a_member_with_access_may_read(self):
        key = self.upload("probe.png", PNG, "image/png")
        self.assertEqual(self.fetch(key, subject=MEMBER).status_code, 200)

    def test_revoking_access_ends_the_reads(self):
        key = self.upload("probe.png", PNG, "image/png")
        self.assertEqual(self.fetch(key, subject=MEMBER).status_code, 200)

        service.revoke_access(self.document_id, MEMBER)
        response = self.fetch(key, subject=MEMBER)
        self.assertEqual(response.status_code, 403)
        self.assertEqual(response.json()["error"], "forbidden")

    def test_a_stranger_is_refused(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key, subject="neev:7:404")
        self.assertEqual(response.status_code, 403)

    def test_another_organization_is_refused(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key, subject=OUTSIDER)
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()["error"], "organization_mismatch")

    def test_the_service_principal_cannot_read_through_this_endpoint(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key, subject=SERVICE_SUBJECT)
        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.json()["error"], "invalid_subject")

    def test_a_missing_subject_is_refused(self):
        key = self.upload("probe.png", PNG, "image/png")
        self.assertEqual(self.fetch(key, subject=None).status_code, 400)

    def test_without_the_bridge_credential_nothing_is_served(self):
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key, key_header=False)
        self.assertEqual(response.status_code, 401)

    def test_a_key_belonging_to_another_document_is_refused(self):
        """The key names its own document; asking through a second one fails."""
        other_id = new_id()
        service.provision_document(other_id, "Other", OWNER)
        key = self.upload("probe.png", PNG, "image/png")
        response = self.fetch(key, document_id=other_id)
        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.json()["error"], "invalid_key")

    def test_malformed_keys_are_refused(self):
        self.upload("probe.png", PNG, "image/png")
        for key in [
            None,
            "",
            f"{self.document_id}/attachments/../../etc/passwd",
            f"{self.document_id}/attachments/nested/file.png",
            f"{self.document_id}/other-folder/file.png",
            f"{self.document_id}/attachments/",
            "../secret",
            f"{self.document_id}//attachments/file.png",
        ]:
            with self.subTest(key=key):
                self.assertEqual(self.fetch(key).status_code, 400)

    def test_an_unrecorded_attachment_is_not_fetchable(self):
        """Well-shaped, never uploaded: the bucket is not a namespace to browse."""
        key = f"{self.document_id}/attachments/{uuid.uuid4()}.png"
        response = self.fetch(key)
        self.assertEqual(response.status_code, 404)
        self.assertEqual(response.json()["error"], "unknown_attachment")

    def test_an_attachment_removed_from_the_document_stops_being_readable(self):
        key = self.upload("probe.png", PNG, "image/png")
        self.assertEqual(self.fetch(key).status_code, 200)

        document = models.Document.objects.get(pk=self.document_id)
        document.attachments.remove(key)
        document.save(update_fields=["attachments"])

        self.assertEqual(self.fetch(key).status_code, 404)


class TitleProjectionTests(TestCase):
    """Neev's title, forced onto the Docs copy after a divergence."""

    def setUp(self):
        self.document_id = new_id()
        self.document, _ = service.provision_document(self.document_id, "Original", OWNER)

    def reread(self):
        return models.Document.objects.get(pk=self.document_id).title

    def test_title_is_projected(self):
        _document, changed = service.set_title(self.document_id, "Renamed in Neev", OWNER)
        self.assertTrue(changed)
        self.assertEqual(self.reread(), "Renamed in Neev")

    def test_projection_is_idempotent(self):
        service.set_title(self.document_id, "Renamed", OWNER)
        _document, changed = service.set_title(self.document_id, "Renamed", OWNER)
        self.assertFalse(changed)
        self.assertEqual(self.reread(), "Renamed")

    def test_last_projection_wins_so_neev_can_always_repair(self):
        """Two repairs in a row converge on the newest native title."""
        service.set_title(self.document_id, "First", OWNER)
        service.set_title(self.document_id, "Second", OWNER)
        self.assertEqual(self.reread(), "Second")

    def test_an_over_long_title_is_bounded_not_rejected(self):
        """Neev's limit is not Docs' 255; the projection must not fail on that."""
        service.set_title(self.document_id, "x" * 400, OWNER)
        self.assertEqual(len(self.reread()), 255)

    def test_an_empty_title_clears_it(self):
        service.set_title(self.document_id, "   ", OWNER)
        self.assertIsNone(self.reread())

    def test_a_non_string_title_is_refused(self):
        with self.assertRaises(service.BridgeError) as caught:
            service.set_title(self.document_id, 42, OWNER)
        self.assertEqual(caught.exception.code, "invalid_title")
        self.assertEqual(self.reread(), "Original")

    def test_another_organization_cannot_rename(self):
        with self.assertRaises(service.BridgeError) as caught:
            service.set_title(self.document_id, "Hostile", OUTSIDER)
        self.assertEqual(caught.exception.code, "organization_mismatch")
        self.assertEqual(self.reread(), "Original")

    def test_unknown_document_is_a_404(self):
        with self.assertRaises(service.BridgeError) as caught:
            service.set_title(new_id(), "Anything", OWNER)
        self.assertEqual(caught.exception.status, 404)

    def test_over_http_with_the_bridge_key(self):
        response = self.client.post(
            f"/_neev/v1/documents/{self.document_id}/title",
            data=json.dumps({"sub": OWNER, "title": "Via HTTP"}),
            content_type="application/json",
            secure=True,
            **{BRIDGE_KEY_HEADER: settings.NEEV_BRIDGE_KEY},
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"title": "Via HTTP", "changed": True})
        self.assertEqual(self.reread(), "Via HTTP")

    def test_without_the_bridge_key_nothing_changes(self):
        response = self.client.post(
            f"/_neev/v1/documents/{self.document_id}/title",
            data=json.dumps({"sub": OWNER, "title": "Forged"}),
            content_type="application/json",
            secure=True,
        )
        self.assertEqual(response.status_code, 401)
        self.assertEqual(self.reread(), "Original")


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

    def test_users_me_reports_onboarding_as_already_done(self):
        """Docs' onboarding modal must never mount in the embed.

        `Auth.tsx:117` renders `<FirstConnection/>` whenever `/users/me/` reports
        `is_first_connection`, and upstream's model defaults it to True. Closing
        that modal calls `users/onboarding-done/`, which the proxy's allowlist
        does not contain, so it would reappear on every load with no way to
        dismiss it — and its slides reference `/assets/on-boarding` without the
        engine prefix, so they would 404 regardless. Onboarding is Neev's.
        """
        response = self.get_me(Client(), OWNER)
        self.assertEqual(response.status_code, 200)
        self.assertFalse(response.json()["is_first_connection"])

    def test_an_account_predating_the_fix_is_normalised(self):
        user = make_user(MEMBER, is_first_connection=True)
        self.assertTrue(user.is_first_connection)

        response = self.get_me(Client(), MEMBER)
        self.assertFalse(response.json()["is_first_connection"])
        user.refresh_from_db()
        self.assertFalse(user.is_first_connection)

    def test_a_non_bridge_account_keeps_upstream_behaviour(self):
        """Only subjects the bridge resolves are touched."""
        stranger = make_user("some-oidc-sub", is_first_connection=True)
        self.get_me(Client(), OWNER)
        stranger.refresh_from_db()
        self.assertTrue(stranger.is_first_connection)

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
