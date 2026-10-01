"""`POST /_neev/v1/documents/<id>/markdown`, inside the Docs image.

The extraction rules themselves (caps, hash, redaction, fail-closed converter
handling) are covered without the image in `tests/test_markdown.py`. This file
covers what needs `core`: the document lookup and its three refusals, the real
`document.content` storage path, the real converter class's exception types,
the route, the credential and the response shape the Neev client depends on.

The converter's network call is the only thing replaced: y-provider is not
part of `run.sh`'s `--no-deps` container, and conversion fidelity is a separate,
fixture-only check (`acceptance/markdown-fidelity.mjs`).

Runs with the rest of the in-image suite: `tests-container/run.sh`.
"""

import base64
import hashlib
import json
import uuid
from unittest import mock

from django.conf import settings
from django.contrib.auth import get_user_model
from django.test import TestCase, override_settings
from django.urls import resolve
from django.utils import timezone

from botocore.exceptions import ClientError

from core import models
from core.services import converter_services

from neev_bridge import markdown, service

OWNER = "neev:7:1"
ORG = 7
BRIDGE_KEY_HEADER = "HTTP_X_NEEV_BRIDGE_KEY"
CONVERT = "core.services.converter_services.YdocConverter.convert"
CONTENT_RESPONSE = "core.models.Document.get_content_response"
Y_PROVIDER = "http://y-provider:4444/api/"
EMPTY_SHA = hashlib.sha256(b"").hexdigest()


def new_id():
    return str(uuid.uuid4())


@override_settings(Y_PROVIDER_API_BASE_URL=Y_PROVIDER)
class MarkdownEndpointTests(TestCase):
    def setUp(self):
        self.document_id = new_id()
        self.document, _ = service.provision_document(self.document_id, "T", OWNER)

    def store(self, raw):
        content = base64.b64encode(raw).decode()
        self.document.content = content
        self.document.save()
        return content

    def post(self, body, document_id=None, key=True):
        headers = {BRIDGE_KEY_HEADER: settings.NEEV_BRIDGE_KEY} if key else {}
        return self.client.post(
            f"/_neev/v1/documents/{document_id or self.document_id}/markdown",
            data=json.dumps(body),
            content_type="application/json",
            secure=True,
            **headers,
        )

    def test_route_resolves(self):
        self.assertEqual(
            resolve(f"/_neev/v1/documents/{self.document_id}/markdown").url_name,
            "neev-bridge-markdown",
        )

    def test_requires_the_bridge_key(self):
        with mock.patch(CONVERT) as convert:
            missing = self.post({"org_id": ORG}, key=False)
            wrong = self.client.post(
                f"/_neev/v1/documents/{self.document_id}/markdown",
                data=json.dumps({"org_id": ORG}),
                content_type="application/json",
                secure=True,
                **{BRIDGE_KEY_HEADER: "wrong" * 10},
            )
        self.assertEqual(missing.status_code, 401)
        self.assertEqual(wrong.status_code, 401)
        convert.assert_not_called()

    def test_only_post(self):
        response = self.client.get(
            f"/_neev/v1/documents/{self.document_id}/markdown",
            secure=True,
            **{BRIDGE_KEY_HEADER: settings.NEEV_BRIDGE_KEY},
        )
        self.assertEqual(response.status_code, 405)

    def test_converts_and_replaces_images(self):
        content = self.store(b"yjs-bytes")
        with mock.patch(CONVERT, return_value="# T\n\n![a](/media/x.png)\n\ntext") as convert:
            response = self.post({"org_id": ORG, "if_none_match": None})
        self.assertEqual(response.status_code, 200)
        body = response.json()
        self.assertEqual(body["id"], self.document_id)
        self.assertIs(body["unchanged"], False)
        self.assertEqual(body["markdown"], "# T\n\n[image]\n\ntext")
        self.assertEqual(body["content_sha256"], hashlib.sha256(content.encode()).hexdigest())
        self.assertTrue(body["updated_at"])
        convert.assert_called_once()
        self.assertEqual(convert.call_args.args[0], b"yjs-bytes")
        self.assertEqual(response["Cache-Control"], "private, no-store")
        self.assertNotIn("docs_sessionid", response.cookies)

    def test_unchanged_skips_conversion(self):
        content = self.store(b"yjs")
        sha = hashlib.sha256(content.encode()).hexdigest()
        with mock.patch(CONVERT) as convert:
            response = self.post({"org_id": ORG, "if_none_match": sha})
        self.assertEqual(
            response.json(),
            {"id": self.document_id, "content_sha256": sha, "unchanged": True},
        )
        convert.assert_not_called()

    def test_empty_document_is_empty_markdown(self):
        with mock.patch(CONVERT) as convert:
            response = self.post({"org_id": ORG})
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["markdown"], "")
        self.assertEqual(response.json()["content_sha256"], EMPTY_SHA)
        convert.assert_not_called()

    def test_org_mismatch_unknown_and_foreign_document(self):
        mismatch = self.post({"org_id": ORG + 1})
        self.assertEqual((mismatch.status_code, mismatch.json()["error"]), (409, "organization_mismatch"))

        unknown = self.post({"org_id": ORG}, document_id=new_id())
        self.assertEqual((unknown.status_code, unknown.json()["error"]), (404, "unknown_document"))

        user = get_user_model()(sub="other:1:1")
        user.set_unusable_password()
        user.save()
        foreign = models.Document.add_root(title="Foreign", creator=user)
        refused = self.post({"org_id": ORG}, document_id=str(foreign.id))
        self.assertEqual((refused.status_code, refused.json()["error"]), (409, "not_an_engine_document"))

    def test_malformed_requests_are_400(self):
        for body in ({}, {"org_id": "7"}, {"org_id": True}, {"org_id": ORG, "if_none_match": "x"}):
            with self.subTest(body=body):
                response = self.post(body)
                self.assertEqual(response.status_code, 400)
                self.assertEqual(response.json()["error"], "invalid_request")
        self.assertEqual(self.post({"org_id": ORG}, document_id="not-a-uuid").status_code, 400)

    def test_converter_failure_is_503_never_empty(self):
        self.store(b"yjs")
        for err in (
            converter_services.ServiceUnavailableError("x"),
            converter_services.ValidationError("x"),
        ):
            with self.subTest(err=type(err).__name__):
                with mock.patch(CONVERT, side_effect=err):
                    response = self.post({"org_id": ORG})
                self.assertEqual(response.status_code, 503)
                self.assertEqual(response.json()["error"], "converter_unavailable")
                self.assertNotIn("markdown", response.json())

    @override_settings(Y_PROVIDER_API_BASE_URL=None)
    def test_an_unconfigured_converter_is_503_and_never_called(self):
        self.store(b"yjs")
        with mock.patch(CONVERT) as convert:
            response = self.post({"org_id": ORG})
        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.json(), {"error": "converter_unavailable", "detail": "converter_not_configured"})
        convert.assert_not_called()

    def test_size_caps(self):
        self.document.content = "A" * (markdown.MAX_CONTENT_B64 + 4)
        self.document.save()
        with mock.patch(CONVERT) as convert:
            response = self.post({"org_id": ORG})
        self.assertEqual((response.status_code, response.json()["error"]), (413, "too_large"))
        convert.assert_not_called()

    def test_oversize_markdown_is_413(self):
        self.store(b"yjs")
        with mock.patch(CONVERT, return_value="x" * (markdown.MAX_MARKDOWN + 1)):
            response = self.post({"org_id": ORG})
        self.assertEqual((response.status_code, response.json()["error"]), (413, "too_large"))

    def test_no_media_url_reaches_the_response(self):
        self.store(b"yjs")
        key = f"{self.document_id}/attachments/k.pdf"
        with mock.patch(
            CONVERT,
            return_value=f"[file.pdf](/media/{key})\n\n<img src=\"/media/{key}\">",
        ):
            response = self.post({"org_id": ORG})
        self.assertEqual(response.status_code, 200)
        self.assertNotIn("attachments/", response.content.decode())
        self.assertNotIn("/media/", response.content.decode())

    def storage_error(self, code):
        return ClientError({"Error": {"Code": code, "Message": "probe"}}, "GetObject")

    def test_a_storage_failure_is_503_never_empty(self):
        """I1: upstream's `content` property would have returned None here."""
        self.store(b"yjs")
        for code in ("InternalError", "AccessDenied", "SlowDown", "NoSuchBucket"):
            with self.subTest(code=code):
                with mock.patch(CONTENT_RESPONSE, side_effect=self.storage_error(code)), \
                        mock.patch(CONVERT) as convert:
                    response = self.post({"org_id": ORG})
                self.assertEqual(response.status_code, 503)
                self.assertEqual(
                    response.json(),
                    {"error": "converter_unavailable", "detail": "content_unavailable"},
                )
                convert.assert_not_called()

    def test_a_missing_object_is_a_never_saved_document(self):
        for err in (self.storage_error("NoSuchKey"), FileNotFoundError()):
            with self.subTest(err=repr(err)):
                with mock.patch(CONTENT_RESPONSE, side_effect=err):
                    response = self.post({"org_id": ORG})
                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.json()["markdown"], "")
                self.assertEqual(response.json()["content_sha256"], EMPTY_SHA)

    def test_the_stored_object_is_read_through_get_content_response(self):
        """The real storage path, end to end: what was saved is what is hashed."""
        content = self.store(b"yjs-through-storage")
        with mock.patch(CONVERT, return_value="ok") as convert:
            response = self.post({"org_id": ORG})
        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json()["content_sha256"], hashlib.sha256(content.encode()).hexdigest()
        )
        self.assertEqual(convert.call_args.args[0], b"yjs-through-storage")

    def test_a_soft_deleted_document_is_unknown(self):
        """M1: in Docs' trash, or under a trashed ancestor, is not a source."""
        self.store(b"yjs")
        for field in ("deleted_at", "ancestors_deleted_at"):
            with self.subTest(field=field):
                models.Document.objects.filter(pk=self.document_id).update(
                    deleted_at=None, ancestors_deleted_at=None
                )
                models.Document.objects.filter(pk=self.document_id).update(
                    **{field: timezone.now()}
                )
                with mock.patch(CONVERT) as convert:
                    response = self.post({"org_id": ORG})
                self.assertEqual(
                    (response.status_code, response.json()), (404, {"error": "unknown_document"})
                )
                convert.assert_not_called()
