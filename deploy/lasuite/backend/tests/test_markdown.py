"""Markdown extraction rules, without the Docs image.

`neev_bridge.markdown` holds everything about the markdown endpoint that does
not need `core`: request validation, the size caps, the content hash, the
converter call's failure handling, and media redaction. The document lookup and
the real converter are exercised in `tests-container/tests_markdown.py`.

The converter is passed in as a callable, so these tests drive the same code
path production does and only replace the network hop to y-provider.
"""

import base64
import hashlib

from django.test import SimpleTestCase

from neev_bridge import markdown
from neev_bridge.markdown import ExtractionRefused

YJS = base64.b64encode(b"yjs-bytes").decode()
SHA_OF_YJS = hashlib.sha256(YJS.encode()).hexdigest()
SHA_OF_EMPTY = hashlib.sha256(b"").hexdigest()


def returning(value):
    """A converter that records what it was given and returns `value`."""

    def convert(data):
        convert.calls.append(data)
        return value

    convert.calls = []
    return convert


def raising(err):
    def convert(_data):
        raise err

    return convert


class RequestValidationTests(SimpleTestCase):
    def test_org_id_must_be_a_positive_integer(self):
        self.assertEqual(markdown.require_org_id(7), "7")
        for bad in (None, 0, -1, "7", 7.0, True, False, [7]):
            with self.subTest(org_id=bad):
                with self.assertRaises(ExtractionRefused) as caught:
                    markdown.require_org_id(bad)
                self.assertEqual(caught.exception.code, "invalid_request")
                self.assertEqual(caught.exception.status, 400)

    def test_if_none_match_is_null_or_a_lowercase_sha256(self):
        self.assertIsNone(markdown.require_tag(None))
        self.assertEqual(markdown.require_tag(SHA_OF_YJS), SHA_OF_YJS)
        for bad in ("", "abc", SHA_OF_YJS.upper(), SHA_OF_YJS + "0", 42, ["x"]):
            with self.subTest(tag=bad):
                with self.assertRaises(ExtractionRefused) as caught:
                    markdown.require_tag(bad)
                self.assertEqual(caught.exception.status, 400)


class ExtractTests(SimpleTestCase):
    def test_converts_the_decoded_yjs_and_hashes_the_stored_text(self):
        convert = returning("# T\n\ntext")
        result = markdown.extract(YJS, None, convert)
        self.assertEqual(convert.calls, [b"yjs-bytes"])
        self.assertEqual(
            result,
            {"content_sha256": SHA_OF_YJS, "unchanged": False, "markdown": "# T\n\ntext"},
        )

    def test_a_matching_tag_skips_conversion(self):
        convert = returning("never")
        result = markdown.extract(YJS, SHA_OF_YJS, convert)
        self.assertEqual(result, {"content_sha256": SHA_OF_YJS, "unchanged": True})
        self.assertEqual(convert.calls, [])

    def test_a_stale_tag_converts(self):
        convert = returning("new")
        result = markdown.extract(YJS, SHA_OF_EMPTY, convert)
        self.assertFalse(result["unchanged"])
        self.assertEqual(result["markdown"], "new")

    def test_no_stored_content_is_empty_markdown_without_a_converter_call(self):
        for stored in (None, ""):
            with self.subTest(stored=stored):
                convert = returning("never")
                result = markdown.extract(stored, None, convert)
                self.assertEqual(
                    result,
                    {"content_sha256": SHA_OF_EMPTY, "unchanged": False, "markdown": ""},
                )
                self.assertEqual(convert.calls, [])

    def test_bytes_from_the_converter_are_decoded(self):
        result = markdown.extract(YJS, None, returning("é".encode()))
        self.assertEqual(result["markdown"], "é")

    def test_images_are_redacted_in_the_result(self):
        result = markdown.extract(YJS, None, returning("# T\n\n![a](/media/x.png)\n\ntext"))
        self.assertEqual(result["markdown"], "# T\n\n[image]\n\ntext")


class FailClosedTests(SimpleTestCase):
    """A converter problem is never reported as an empty document."""

    def assertRefused(self, code, status, call):
        with self.assertRaises(ExtractionRefused) as caught:
            call()
        self.assertEqual(caught.exception.code, code)
        self.assertEqual(caught.exception.status, status)
        return caught.exception

    def test_any_converter_exception_is_503(self):
        for err in (RuntimeError("down"), ValueError("bad"), TimeoutError()):
            with self.subTest(err=type(err).__name__):
                self.assertRefused(
                    "converter_unavailable", 503,
                    lambda err=err: markdown.extract(YJS, None, raising(err)),
                )

    def test_a_non_text_result_is_503(self):
        for value in (None, 42, {"markdown": "x"}, b"\xff\xfe"):
            with self.subTest(value=value):
                self.assertRefused(
                    "converter_unavailable", 503,
                    lambda value=value: markdown.extract(YJS, None, returning(value)),
                )

    def test_stored_content_that_is_not_base64_is_503_and_says_so(self):
        err = self.assertRefused(
            "converter_unavailable", 503,
            lambda: markdown.extract("not base64!!", None, returning("x")),
        )
        self.assertEqual(err.detail, "invalid_stored_content")

    def test_a_refusal_raised_by_the_converter_keeps_its_own_detail(self):
        """The service raises `converter_not_configured` from inside the call."""
        refused = ExtractionRefused("converter_unavailable", 503, "converter_not_configured")
        err = self.assertRefused(
            "converter_unavailable", 503, lambda: markdown.extract(YJS, None, raising(refused))
        )
        self.assertEqual(err.detail, "converter_not_configured")

    def test_empty_and_unchanged_never_need_the_converter(self):
        refused = ExtractionRefused("converter_unavailable", 503, "converter_not_configured")
        self.assertEqual(markdown.extract("", None, raising(refused))["markdown"], "")
        self.assertTrue(markdown.extract(YJS, SHA_OF_YJS, raising(refused))["unchanged"])

    def test_the_failure_detail_never_carries_converter_text(self):
        err = self.assertRefused(
            "converter_unavailable", 503,
            lambda: markdown.extract(YJS, None, raising(RuntimeError("SECRET body"))),
        )
        self.assertNotIn("SECRET", str(err.detail))


class SizeCapTests(SimpleTestCase):
    def test_oversize_stored_content_is_413_before_any_conversion(self):
        convert = returning("x")
        stored = "A" * (markdown.MAX_CONTENT_B64 + 4)
        with self.assertRaises(ExtractionRefused) as caught:
            markdown.extract(stored, None, convert)
        self.assertEqual((caught.exception.code, caught.exception.status), ("too_large", 413))
        self.assertEqual(convert.calls, [])

    def test_oversize_is_reported_even_when_the_tag_matches(self):
        """Honest: a source that is too large is never reported as unchanged."""
        stored = "A" * (markdown.MAX_CONTENT_B64 + 4)
        tag = hashlib.sha256(stored.encode()).hexdigest()
        with self.assertRaises(ExtractionRefused) as caught:
            markdown.extract(stored, tag, returning("x"))
        self.assertEqual(caught.exception.status, 413)

    def test_content_at_the_cap_is_accepted(self):
        stored = "A" * markdown.MAX_CONTENT_B64
        result = markdown.extract(stored, None, returning("ok"))
        self.assertEqual(result["markdown"], "ok")

    def test_oversize_markdown_is_413_not_truncated(self):
        big = "x" * (markdown.MAX_MARKDOWN + 1)
        with self.assertRaises(ExtractionRefused) as caught:
            markdown.extract(YJS, None, returning(big))
        self.assertEqual((caught.exception.code, caught.exception.status), ("too_large", 413))

    def test_the_markdown_cap_counts_utf8_bytes(self):
        # 3 bytes per character: under the cap in characters, over it in bytes.
        big = "€" * (markdown.MAX_MARKDOWN // 3 + 1)
        with self.assertRaises(ExtractionRefused):
            markdown.extract(YJS, None, returning(big))

    def test_markdown_exactly_at_the_cap_is_accepted(self):
        exact = "x" * markdown.MAX_MARKDOWN
        self.assertEqual(markdown.extract(YJS, None, returning(exact))["markdown"], exact)


class RedactionTests(SimpleTestCase):
    """No media key, signed URL or attachment path reaches an agent."""

    def assertClean(self, text):
        for needle in ("/media/", "attachments/", "X-Amz", "data:image", "http"):
            self.assertNotIn(needle, text)

    def test_markdown_images_become_placeholders(self):
        cases = {
            "![a](/media/x.png)": "[image]",
            "![](https://e.example/p.png)": "[image]",
            '![cap](/media/x.png "title")': "[image]",
            "![a [nested] b](/media/x.png)": "[image]",
            "![a](</media/with space.png>)": "[image]",
            "![a](/media/x_(1).png)": "[image]",
            "![a\\]b](/media/x.png)": "[image]",
            "![ref][img1]": "[image]",
        }
        for source, expected in cases.items():
            with self.subTest(source=source):
                self.assertEqual(markdown.redact_media(source), expected)

    def test_surrounding_text_is_kept(self):
        self.assertEqual(
            markdown.redact_media("before ![a](/media/x.png) after"),
            "before [image] after",
        )

    def test_html_media_tags_are_replaced(self):
        out = markdown.redact_media(
            '<img src="/media/a.png" alt="x"> and '
            '<video src="/media/v.mp4"></video> and '
            '<audio controls><source src="/media/s.mp3"></audio>'
        )
        self.assertIn("[image]", out)
        self.assertClean(out)

    def test_links_to_attachments_keep_their_name_but_lose_the_url(self):
        doc_id = "4c0c8293-0000-4000-8000-000000000000"
        out = markdown.redact_media(
            f"see [pricing.pdf](http://backend:8000/media/{doc_id}/attachments/k.pdf)"
        )
        self.assertEqual(out, "see [attachment: pricing.pdf]")

    def test_a_link_whose_text_is_the_media_url_leaks_nothing(self):
        url = "/media/4c0c8293/attachments/k.pdf"
        out = markdown.redact_media(f"[{url}]({url})")
        self.assertEqual(out, "[attachment]")

    def test_ordinary_links_are_left_alone(self):
        text = "read [the guide](https://example.com/guide) first"
        self.assertEqual(markdown.redact_media(text), text)

    def test_bare_media_urls_and_keys_are_swept(self):
        for source in (
            "raw /media/4c0c8293/attachments/k.png here",
            "raw http://host/document-engine/media/k.png here",
            "raw https://host/media-auth/x here",
            "raw 4c0c8293-0000-4000-8000-000000000000/attachments/abc.png here",
            "raw https://s3.example/b/k?X-Amz-Signature=abc&X-Amz-Credential=y here",
            "raw /_neev/v1/documents/4c0c8293/media?key=k here",
            "raw data:image/png;base64,iVBORw0KGgo= here",
            "<https://host/media/k.png>",
        ):
            with self.subTest(source=source):
                out = markdown.redact_media(source)
                self.assertClean(out)

    def test_reference_definitions_pointing_at_media_are_dropped(self):
        out = markdown.redact_media("![x][1]\n\n[1]: /media/k.png\n")
        self.assertClean(out)
        self.assertIn("[image]", out)

    def test_plain_text_and_structure_survive(self):
        text = (
            "# Fidelity\n\n- one\n- two\n\n| a | b |\n| --- | --- |\n| c | d |\n\n"
            "a commented paragraph\n"
        )
        self.assertEqual(markdown.redact_media(text), text)

    def test_redaction_is_linear_on_hostile_input(self):
        """No catastrophic backtracking on unterminated constructs."""
        import time

        cases = {
            "brackets": "![" + "[" * 20000 + "](" * 20000,
            # One long word: a pattern anchored anywhere inside it rescans it.
            "long word": "x" * markdown.MAX_MARKDOWN,
            "unclosed tags": "<img " * 200000,
            "slashes": "a/" * 500000,
        }
        for name, hostile in cases.items():
            with self.subTest(case=name):
                started = time.monotonic()
                markdown.redact_media(hostile)
                self.assertLess(time.monotonic() - started, 2.0)

    def test_an_unclosed_image_tag_still_loses_its_url(self):
        self.assertClean(markdown.redact_media('<img src="/media/k.png" '))


class FakeBody:
    """A `StreamingBody` stand-in that records how much was asked of it."""

    def __init__(self, data, fail=None):
        self.data = data
        self.offset = 0
        self.read_bytes = 0
        self.closed = False
        self.fail = fail

    def read(self, amt=None):
        if self.fail:
            raise self.fail
        if amt is None:
            raise AssertionError("unbounded read")
        chunk = self.data[self.offset : self.offset + amt]
        self.offset += len(chunk)
        self.read_bytes += len(chunk)
        return chunk

    def close(self):
        self.closed = True


class Missing(Exception):
    pass


def opener(body=None, length="auto", err=None):
    def open_object():
        if err is not None:
            raise err
        response = {"Body": body}
        if length == "auto":
            response["ContentLength"] = len(body.data)
        elif length is not None:
            response["ContentLength"] = length
        return response

    return open_object


def is_missing(err):
    return isinstance(err, Missing)


class StoredContentReadTests(SimpleTestCase):
    """I1/M2: the stored object is read here, bounded, and misread never means ""."""

    def read(self, open_object):
        return markdown.read_stored_content(open_object, is_missing)

    def assertRefused(self, code, status, detail, call):
        with self.assertRaises(ExtractionRefused) as caught:
            call()
        self.assertEqual(
            (caught.exception.code, caught.exception.status, caught.exception.detail),
            (code, status, detail),
        )

    def test_the_stored_text_is_returned(self):
        body = FakeBody(YJS.encode())
        self.assertEqual(self.read(opener(body)), YJS)
        self.assertTrue(body.closed)

    def test_a_missing_object_is_a_never_saved_document(self):
        self.assertEqual(self.read(opener(err=Missing())), "")

    def test_every_other_storage_error_is_503_not_empty(self):
        for err in (RuntimeError("InternalError"), PermissionError(), OSError("SlowDown")):
            with self.subTest(err=type(err).__name__):
                self.assertRefused(
                    "converter_unavailable", 503, "content_unavailable",
                    lambda err=err: self.read(opener(err=err)),
                )

    def test_a_body_that_fails_mid_read_is_503(self):
        body = FakeBody(b"", fail=ConnectionResetError())
        self.assertRefused(
            "converter_unavailable", 503, "content_unavailable",
            lambda: self.read(opener(body, length=10)),
        )
        self.assertTrue(body.closed)

    def test_a_declared_oversize_object_is_413_without_reading_it(self):
        body = FakeBody(b"A" * 10)
        self.assertRefused(
            "too_large", 413, None,
            lambda: self.read(opener(body, length=markdown.MAX_CONTENT_B64 + 1)),
        )
        self.assertEqual(body.read_bytes, 0)
        self.assertTrue(body.closed)

    def test_an_undeclared_oversize_object_stops_one_chunk_past_the_cap(self):
        body = FakeBody(b"A" * (markdown.MAX_CONTENT_B64 * 4))
        self.assertRefused(
            "too_large", 413, None, lambda: self.read(opener(body, length=None))
        )
        self.assertLessEqual(
            body.read_bytes, markdown.MAX_CONTENT_B64 + markdown.READ_CHUNK_BYTES
        )

    def test_an_object_exactly_at_the_cap_is_read_whole(self):
        body = FakeBody(b"A" * markdown.MAX_CONTENT_B64)
        self.assertEqual(len(self.read(opener(body, length=None))), markdown.MAX_CONTENT_B64)

    def test_non_ascii_stored_content_is_503(self):
        self.assertRefused(
            "converter_unavailable", 503, "invalid_stored_content",
            lambda: self.read(opener(FakeBody("é".encode()))),
        )

    def test_a_response_without_a_body_is_503(self):
        self.assertRefused(
            "converter_unavailable", 503, "content_unavailable",
            lambda: self.read(lambda: {"ContentLength": 3}),
        )


class RedactionPostCheckTests(SimpleTestCase):
    """M5: if redaction ever misses, refuse rather than return the text."""

    def test_a_residual_media_reference_refuses_the_whole_result(self):
        from unittest import mock

        leaky = "text /media/4c0c8293/attachments/k.png"
        with mock.patch.object(markdown, "redact_media", side_effect=lambda text: text):
            with self.assertRaises(ExtractionRefused) as caught:
                markdown.extract(YJS, None, returning(leaky))
        self.assertEqual(
            (caught.exception.code, caught.exception.status, caught.exception.detail),
            ("converter_unavailable", 503, "redaction_incomplete"),
        )

    def test_residue_detection(self):
        for leaky in (
            "/media/k.png",
            "/media-auth/k",
            "x/media?key=k",
            "4c0c8293-0000-4000-8000-000000000000/attachments/k",
            "https://s3/b/k?X-Amz-Signature=1",
            "data:image/png;base64,AAAA",
        ):
            with self.subTest(leaky=leaky):
                self.assertTrue(markdown.has_media_residue(leaky))

    def test_clean_redacted_text_has_no_residue(self):
        for text in (
            "# T\n\n[image]\n\n[attachment: a.pdf] [media]",
            "metadata:text/plain is fine without a comma",
            "see [the guide](https://example.com/guide)",
        ):
            with self.subTest(text=text):
                self.assertFalse(markdown.has_media_residue(text))
