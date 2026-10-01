"""Server-side markdown for Neev Knowledge: the rules that do not need `core`.

The endpoint (`views.document_markdown`) resolves the document and hands its
stored content here together with a converter callable. Everything about the
answer is decided in this module:

* the request's own fields are validated, so a malformed call is a 400 rather
  than a lookup;
* the stored content is capped *before* it is hashed or converted, and an
  oversize source is refused as `too_large` — never truncated, and never
  reported as unchanged, so Neev can tell the member why it was left out;
* the content hash is over the stored base64 text, which is what Neev compares
  against on the next call (`if_none_match`);
* any converter failure — an exception, a non-text result, undecodable stored
  content — is `converter_unavailable`. It is never turned into an empty
  document, because an empty document is a statement about the source and a
  converter outage is not;
* the stored object is read here, not through upstream's `Document.content`,
  which turns *every* storage error into `None` — an outage would read as an
  empty document. Only a missing object is a never-saved (empty) document; any
  other failure is a refusal. The read is bounded in bytes and in time;
* media is redacted. Images become `[image]`, attachment links lose their URL,
  and any media path, attachment key, signed URL or data URI left anywhere in
  the text is replaced. An agent reading this text must not be handed a key it
  could use to ask for the bytes. Redaction is then checked: if anything
  media-shaped survived it, the whole result is refused.

Pure Python plus the standard library, so it is tested without the Docs image.
"""

import base64
import binascii
import hashlib
import logging
import re
import time

logger = logging.getLogger(__name__)

# Stored Yjs is base64 text; 2 MiB of it is a large document by any measure.
MAX_CONTENT_B64 = 2 * 1024 * 1024
# What Neev will accept as one source.
MAX_MARKDOWN = 1024 * 1024
# The converter's raw output is bounded before redaction runs over it, so a
# pathological result cannot buy unbounded regex work.
MAX_RAW_MARKDOWN_CHARS = 4 * MAX_MARKDOWN
# The stored object is read in chunks, so an undeclared oversize object costs
# at most one chunk past the cap, and a stalled store cannot hold the worker.
READ_CHUNK_BYTES = 64 * 1024
READ_DEADLINE_SECONDS = 30.0

_SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")


class ExtractionRefused(Exception):
    """A refusal with the same `(code, status, detail)` shape as `BridgeError`."""

    def __init__(self, code, status, detail=None):
        super().__init__(code)
        self.code = code
        self.status = status
        self.detail = detail


def require_org_id(value):
    """The organization the caller is asking on behalf of, as a subject segment.

    An integer, strictly: `True` is an `int` to Python and must not become org 1.
    """
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ExtractionRefused("invalid_request", 400, "org_id must be a positive integer")
    return str(value)


def require_tag(value):
    """`if_none_match` is null or the lowercase hex sha256 a previous call returned."""
    if value is None:
        return None
    if not isinstance(value, str) or not _SHA256_HEX.match(value):
        raise ExtractionRefused(
            "invalid_request", 400, "if_none_match must be null or a sha256 hex digest"
        )
    return value


def _unavailable(detail, err=None):
    if err is not None:
        # The type only: storage errors can carry keys and bucket names.
        logger.warning("neev-bridge: stored content unreadable (%s)", type(err).__name__)
    return ExtractionRefused("converter_unavailable", 503, detail)


def read_stored_content(open_object, is_missing):
    """The stored base64 text, read with bounds, or a refusal.

    `open_object()` returns a `get_object`-shaped response (`Body`, optional
    `ContentLength`); `is_missing(err)` says whether an exception it raised
    means "no object was ever saved". That, and only that, is "".
    """
    try:
        response = open_object()
    except Exception as err:  # noqa: BLE001 - classified below, never swallowed
        if is_missing(err):
            return ""
        raise _unavailable("content_unavailable", err) from err

    body = response.get("Body") if isinstance(response, dict) else None
    try:
        if body is None:
            raise _unavailable("content_unavailable")
        length = response.get("ContentLength")
        if isinstance(length, int) and length > MAX_CONTENT_B64:
            raise ExtractionRefused("too_large", 413)

        chunks, size, started = [], 0, time.monotonic()
        while True:
            try:
                chunk = body.read(READ_CHUNK_BYTES)
            except Exception as err:  # noqa: BLE001 - a broken read is not ""
                raise _unavailable("content_unavailable", err) from err
            if not chunk:
                break
            chunks.append(chunk)
            size += len(chunk)
            if size > MAX_CONTENT_B64:
                raise ExtractionRefused("too_large", 413)
            if time.monotonic() - started > READ_DEADLINE_SECONDS:
                raise _unavailable("content_unavailable")
    finally:
        if body is not None:
            try:
                body.close()
            except Exception:  # noqa: BLE001 - nothing useful to do
                pass

    try:
        # Base64 is ASCII; anything else is not content this bridge wrote.
        return b"".join(chunks).decode("ascii")
    except UnicodeDecodeError as err:
        raise _unavailable("invalid_stored_content") from err


def content_sha256(content):
    return hashlib.sha256(content.encode("utf-8")).hexdigest()


def extract(content, if_none_match, convert):
    """Markdown for one stored document, or a refusal.

    `convert(yjs_bytes)` is the upstream converter call. It is only made when
    the content is non-empty, within the cap, and differs from `if_none_match`.
    """
    content = content or ""
    if not isinstance(content, str):
        raise ExtractionRefused("converter_unavailable", 503, "invalid_stored_content")
    if len(content) > MAX_CONTENT_B64:
        raise ExtractionRefused("too_large", 413)

    sha = content_sha256(content)
    if if_none_match is not None and if_none_match == sha:
        return {"content_sha256": sha, "unchanged": True}

    if not content:
        return {"content_sha256": sha, "unchanged": False, "markdown": ""}

    try:
        data = base64.b64decode(content, validate=True)
    except (binascii.Error, ValueError) as err:
        raise ExtractionRefused(
            "converter_unavailable", 503, "invalid_stored_content"
        ) from err

    try:
        result = convert(data)
    except ExtractionRefused:
        raise
    except Exception as err:  # noqa: BLE001 - every failure is a refusal, not ""
        # The type only: a converter error can echo the document back.
        logger.warning("neev-bridge: markdown conversion failed (%s)", type(err).__name__)
        raise ExtractionRefused("converter_unavailable", 503, "converter_error") from err

    if isinstance(result, bytes):
        try:
            result = result.decode("utf-8")
        except UnicodeDecodeError as err:
            raise ExtractionRefused(
                "converter_unavailable", 503, "converter_returned_non_text"
            ) from err
    if not isinstance(result, str):
        raise ExtractionRefused("converter_unavailable", 503, "converter_returned_non_text")

    if len(result) > MAX_RAW_MARKDOWN_CHARS:
        raise ExtractionRefused("too_large", 413)
    result = redact_media(result)
    if has_media_residue(result):
        # Never seen; exists so a gap in the patterns is a refusal, not a leak.
        logger.warning("neev-bridge: media reference survived redaction")
        raise ExtractionRefused("converter_unavailable", 503, "redaction_incomplete")
    if len(result.encode("utf-8")) > MAX_MARKDOWN:
        raise ExtractionRefused("too_large", 413)

    return {"content_sha256": sha, "unchanged": False, "markdown": result}


# --- media redaction -------------------------------------------------------
#
# Every pattern below is built from alternatives that differ in their first
# character, so none of them can backtrack exponentially on hostile input.

# A link label: escapes, one level of nested brackets, or anything else.
_LABEL = r"(?:\\.|\[[^\[\]\n]*\]|[^\[\]\\\n])*"
# A link destination: `<...>`, or a run with balanced single-level parentheses.
_DEST = r"(?:<[^<>\n]*>|(?:\\.|\([^()\s]*\)|[^()\s\\])+)"
_TITLE = r"""(?:\s+(?:"[^"\n]*"|'[^'\n]*'|\([^()\n]*\)))?"""

# `[^<>]`, not `[^>]`: a run of unclosed tags must not each scan to the end.
# An unclosed tag's URL is still caught by the final sweep.
_HTML_IMAGE = re.compile(r"<img\b[^<>]*>", re.IGNORECASE)
_HTML_MEDIA_OPEN = re.compile(
    r"<(?:video|audio|source|track|iframe|embed|object|picture)\b[^<>]*>", re.IGNORECASE
)
_HTML_MEDIA_CLOSE = re.compile(r"</(?:video|audio|iframe|object|picture)\s*>", re.IGNORECASE)
_IMAGE_INLINE = re.compile(rf"!\[{_LABEL}\]\(\s*{_DEST}?{_TITLE}\s*\)")
_IMAGE_REFERENCE = re.compile(rf"!\[{_LABEL}\](?:\[[^\[\]\n]*\])?")
_LINK = re.compile(rf"\[({_LABEL})\]\(\s*({_DEST}){_TITLE}\s*\)")
_REFERENCE_DEFINITION = re.compile(r"^ {0,3}\[[^\[\]\n]+\]:[ \t]*(\S+)[^\n]*$", re.MULTILINE)
_DATA_URI = re.compile(r"data:[a-z]+/[a-z0-9.+-]+(?:;[a-z0-9=.+-]+)*,[^\s)\]>\"']*", re.IGNORECASE)
# Anything URL- or path-shaped, then judged by `_is_media`. Attachment keys
# (`<document id>/attachments/<file>`) appear without a leading slash; the
# lookbehind starts that alternative only at the beginning of a word, so a long
# word is scanned once rather than once per character.
_URLISH = re.compile(
    r"<(?:https?://|/)[^\s<>]*>"
    r"|(?<![0-9A-Za-z-])[0-9A-Za-z-]+/attachments/[^\s<>()\[\]\"']*"
    r"|(?:https?://|/)[^\s<>()\[\]\"']*",
    re.IGNORECASE,
)
_MEDIA_HINT = re.compile(
    r"/media(?:-auth)?/|/media\?|/attachments/|[?&]X-Amz-|^<?data:", re.IGNORECASE
)


_RESIDUE = re.compile(r"/media(?:-auth)?/|/media\?|/attachments/|[?&]X-Amz-", re.IGNORECASE)


def has_media_residue(text):
    """True if anything media-shaped is left in already-redacted text."""
    return bool(_RESIDUE.search(text) or _DATA_URI.search(text))


def _is_media(url):
    return bool(_MEDIA_HINT.search(url))


def _sweep(text):
    text = _DATA_URI.sub("[media]", text)
    return _URLISH.sub(
        lambda match: "[media]" if _is_media(match.group(0)) else match.group(0), text
    )


def _attachment_link(match):
    if not _is_media(match.group(2)):
        return match.group(0)
    name = _sweep(match.group(1)).strip()
    if not name or "[media]" in name:
        return "[attachment]"
    return f"[attachment: {name}]"


def _reference_definition(match):
    return "" if _is_media(match.group(1)) else match.group(0)


def redact_media(text):
    """Replace every image and media reference; leave all other text alone."""
    text = _HTML_IMAGE.sub("[image]", text)
    text = _HTML_MEDIA_OPEN.sub("[media]", text)
    text = _HTML_MEDIA_CLOSE.sub("", text)
    text = _IMAGE_INLINE.sub("[image]", text)
    text = _IMAGE_REFERENCE.sub("[image]", text)
    text = _LINK.sub(_attachment_link, text)
    text = _REFERENCE_DEFINITION.sub(_reference_definition, text)
    return _sweep(text)
