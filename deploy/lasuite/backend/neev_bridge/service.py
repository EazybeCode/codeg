"""Provisioning and ACL projection against the Docs data model.

Neev's native ACL is authoritative for every request; Docs' own ACL exists only
so that upstream code — including y-provider, which reads `abilities.update` to
decide whether a socket may write — agrees with the decision Neev already made.

Two deliberate choices, both about not impersonating anyone:

* every engine document is owned by one ordinary service principal
  (`neev:service:engine`). Granting access is then an ordinary owner action by
  the principal actually making the request, instead of a request forged in the
  creator's name.
* no human ever holds `owner` or `administrator` here. Projected roles are
  `reader` and `editor` only, so even a Docs-side control that escaped the
  proxy's allowlist could not be used to reconfigure sharing or delete the
  document. Sharing stays a Neev concept.

This module imports `core`, so it is only importable inside the Docs image.
"""

import logging
import time
import uuid

from django.conf import settings
from django.core.exceptions import ValidationError as DjangoValidationError
from django.core.files.storage import default_storage
from django.db import IntegrityError, transaction

from botocore.exceptions import ClientError

from core import choices, enums, models
from core.enums import DocumentAttachmentStatus
from core.utils.s3 import get_s3_client
from core.utils.treebeard import create_tree_node_with_retry

from neev_bridge import markdown as markdown_rules
from neev_bridge.identity import SERVICE_SUBJECT, Subject, parse_subject

logger = logging.getLogger(__name__)

# Roles the bridge is allowed to project. Everything else is refused, including
# `owner`, `administrator` and `commenter`.
PROJECTABLE_ROLES = (choices.RoleChoices.READER, choices.RoleChoices.EDITOR)

# Types rendered in place. Raster only: every one of these is decoded by an image
# decoder and cannot carry script. `image/svg+xml` is deliberately absent — it is
# an image to a browser and a script host to an attacker, and upstream does not
# classify it as unsafe (`DOCUMENT_UNSAFE_MIME_TYPES` has no SVG entry).
INLINE_CONTENT_TYPES = frozenset(
    {"image/png", "image/jpeg", "image/gif", "image/webp", "image/avif"}
)

# Upstream refuses an upload above `DOCUMENT_IMAGE_MAX_SIZE` (10 MB by default);
# this is that with room for a larger configured limit, and it is a ceiling on
# what one request can stream, not a policy about what may be stored.
MAX_MEDIA_BYTES = 12 * 1024 * 1024
MEDIA_CHUNK_BYTES = 64 * 1024
# A stalled object store must not hold a request worker indefinitely.
MEDIA_READ_DEADLINE = 30.0


class BridgeError(Exception):
    """A refusal with a stable machine-readable code."""

    def __init__(self, code, status=400, detail=None):
        super().__init__(code)
        self.code = code
        self.status = status
        self.detail = detail


def parse_document_id(value):
    """Accept only a UUIDv4 — the same shape y-provider requires of a room."""
    if not isinstance(value, str):
        raise BridgeError("invalid_document_id")
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError, TypeError) as err:
        raise BridgeError("invalid_document_id") from err
    if parsed.version != 4:
        raise BridgeError("invalid_document_id", detail="document id must be UUIDv4")
    return parsed


def require_subject(value, allow_service=False):
    """Parse a subject from a request body, refusing the service namespace."""
    subject = parse_subject(value)
    if subject is None or (subject.is_service and not allow_service):
        raise BridgeError("invalid_subject")
    return subject


def clean_title(value):
    """Titles are optional, must be text, and are bounded by the column."""
    if value is None:
        return None
    if not isinstance(value, str):
        raise BridgeError("invalid_title")
    return value.strip()[:255] or None


def document_org_id(document):
    """The organization a document belongs to, read from its creator's subject.

    There is no org column in Docs, and adding one would mean a schema change to
    a product we are running unmodified. The creator's subject already carries
    it, and the bridge sets that creator itself at provisioning time.
    """
    creator = document.creator
    subject = parse_subject(getattr(creator, "sub", None)) if creator else None
    if subject is None:
        raise BridgeError("unowned_document", status=409)
    return subject.org_id


def get_service_principal():
    """The single non-human principal that owns every engine document."""
    # Imported here so `backends` stays importable without `core` present.
    from neev_bridge.backends import get_or_create_user_by_subject

    subject = parse_subject(SERVICE_SUBJECT)
    user, _created = get_or_create_user_by_subject(subject)
    return user


def resolve_bridge_user(subject: Subject, profile=None):
    """Resolve — or create — the Docs user behind a validated subject."""
    from neev_bridge.backends import get_or_create_user_by_subject

    user, _created = get_or_create_user_by_subject(subject, profile)
    return user



def _is_engine_document(document, service_user):
    """True when this document was provisioned by the bridge.

    The bridge refuses to touch anything it did not create, so a stray document
    id cannot be used to grant access to a document that belongs to some other
    (for example, preview) population of the same database.
    """
    return models.DocumentAccess.objects.filter(
        document=document, user=service_user, role=choices.RoleChoices.OWNER
    ).exists()


@transaction.atomic
def provision_document(document_id, title, owner_subject, owner_profile=None):
    """Create the Docs document carrying the Neev resource id. Idempotent.

    Returns `(document, created)`. A second call for the same id is a no-op that
    still repairs missing access rows, so a retry after a partial failure
    converges instead of leaving a document nobody can open.
    """
    document_uuid = parse_document_id(document_id)
    subject = require_subject(owner_subject)

    service_user = get_service_principal()
    creator = resolve_bridge_user(subject, owner_profile)

    title = clean_title(title)

    document = models.Document.objects.filter(pk=document_uuid).first()
    created = False
    if document is None:
        try:
            document = create_tree_node_with_retry(
                lambda: models.Document.add_root(
                    id=document_uuid,
                    title=title,
                    creator=creator,
                    # No link sharing: reachability is Neev's decision, per request.
                    link_reach=choices.LinkReachChoices.RESTRICTED,
                    link_role=choices.LinkRoleChoices.READER,
                )
            )
            created = True
        except (IntegrityError, DjangoValidationError):
            # Two provisioning calls for the same resource id raced — Neev
            # retrying, or two tabs. Whoever lost converges on the row the
            # winner committed instead of surfacing a 500. The savepoint opened
            # by create_tree_node_with_retry has already rolled back, so the
            # surrounding transaction is still usable.
            document = models.Document.objects.filter(pk=document_uuid).first()
            if document is None:
                raise

    if not created and not _is_engine_document(document, service_user):
        raise BridgeError("not_an_engine_document", status=409)
    if not created and document_org_id(document) != subject.org_id:
        # The id exists, but it belongs to another organization's document.
        raise BridgeError("organization_mismatch", status=409)

    models.DocumentAccess.objects.get_or_create(
        document=document,
        user=service_user,
        defaults={"role": choices.RoleChoices.OWNER},
    )
    models.DocumentAccess.objects.get_or_create(
        document=document,
        user=creator,
        defaults={"role": choices.RoleChoices.EDITOR},
    )
    return document, created


class MediaObject:
    """An attachment the caller is allowed to read, ready to stream."""

    __slots__ = ("key", "content_type", "length", "inline", "filename", "chunks")

    def __init__(self, key, content_type, length, inline, filename, chunks):
        self.key = key
        self.content_type = content_type
        self.length = length
        self.inline = inline
        self.filename = filename
        self.chunks = chunks


def _attachment_key(document, key):
    """The key must be this document's attachment, by shape and by record.

    Two checks rather than one. The shape check (`<id>/attachments/…`) is what
    makes a key authorizable as a document at all; the membership check is what
    stops a well-shaped key for a *deleted* or never-recorded attachment from
    being fetched out of the bucket.
    """
    if not isinstance(key, str) or not key:
        raise BridgeError("invalid_key")
    if "\\" in key or "//" in key or "%" in key:
        raise BridgeError("invalid_key")
    if any(segment in {"", ".", ".."} for segment in key.split("/")):
        raise BridgeError("invalid_key")
    prefix = f"{document.id!s}/{enums.ATTACHMENTS_FOLDER}/"
    if not key.startswith(prefix) or "/" in key[len(prefix) :]:
        raise BridgeError("invalid_key")
    if key not in (document.attachments or []):
        # Recorded attachments only: the bucket is not a namespace to browse.
        raise BridgeError("unknown_attachment", status=404)
    return key


def _disposition_for(content_type, key):
    """Inline only for raster images that cannot carry script.

    Upstream marks a dangerous upload by suffixing the key with `-unsafe`, and
    separately stores a `Content-Disposition` — which never survives, because the
    malware-detection callback rewrites the object's metadata with
    `MetadataDirective="REPLACE"` and does not carry it over. So the disposition
    has to be decided here, and it is decided by allowlist: anything not on the
    raster list is a download, including `image/svg+xml`, which is an image to a
    browser and a script host to an attacker.
    """
    stem = key.rsplit("/", 1)[-1].rsplit(".", 1)[0]
    if stem.endswith("-unsafe"):
        return False
    return content_type in INLINE_CONTENT_TYPES


def open_media(document_id, key, subject_value):
    """Authorize an attachment read and return it ready to stream.

    Authorization reuses upstream's own `get_abilities`, so the answer here is
    the same one every other Docs read path gets — this endpoint adds no ACL
    policy of its own, it only refuses earlier and more narrowly.
    """
    document_uuid = parse_document_id(document_id)
    subject = require_subject(subject_value)
    service_user = get_service_principal()
    document = _engine_document_for(document_uuid, service_user, subject)

    key = _attachment_key(document, key)

    user_model = service_user.__class__
    user = user_model.objects.filter(sub=subject.raw).first()
    if user is None or not user.is_active:
        raise BridgeError("forbidden", status=403)
    # Upstream's own permission computation, not a second implementation of it.
    if not document.get_abilities(user).get("retrieve"):
        raise BridgeError("forbidden", status=403)

    client = get_s3_client()
    bucket = default_storage.bucket_name
    try:
        head = client.head_object(Bucket=bucket, Key=key)
    except ClientError as err:
        raise BridgeError("unknown_attachment", status=404) from err

    metadata = {name.lower(): value for name, value in head.get("Metadata", {}).items()}
    # Same readiness rule upstream's media-auth applies: an attachment still
    # being scanned is not readable yet.
    if metadata.get("status", DocumentAttachmentStatus.READY) != (
        DocumentAttachmentStatus.READY
    ):
        raise BridgeError("not_ready", status=409)

    length = head.get("ContentLength")
    if length is None or length > MAX_MEDIA_BYTES:
        raise BridgeError("too_large", status=413)

    content_type = head.get("ContentType") or "application/octet-stream"
    inline = _disposition_for(content_type, key)
    if metadata.get("is_unsafe") == "true":
        inline = False

    return MediaObject(
        key=key,
        content_type=content_type,
        length=length,
        inline=inline,
        filename=key.rsplit("/", 1)[-1],
        chunks=_stream(client, bucket, key, length),
    )


def _stream(client, bucket, key, expected_length):
    """Yield the object in bounded chunks, bounded in time as well as size.

    Both bounds exist because this runs inside a request worker: an object store
    that stalls mid-body would otherwise hold the worker for as long as it liked,
    and a body longer than its own `ContentLength` would be relayed in full.
    """

    def generator():
        started = time.monotonic()
        served = 0
        body = client.get_object(Bucket=bucket, Key=key)["Body"]
        try:
            while True:
                chunk = body.read(MEDIA_CHUNK_BYTES)
                if not chunk:
                    return
                served += len(chunk)
                if served > expected_length or served > MAX_MEDIA_BYTES:
                    logger.warning("neev-bridge: attachment exceeded its own length")
                    return
                if time.monotonic() - started > MEDIA_READ_DEADLINE:
                    logger.warning("neev-bridge: attachment read exceeded its deadline")
                    return
                yield chunk
        finally:
            body.close()

    return generator


@transaction.atomic
def set_title(document_id, title, subject_value):
    """Force the Docs title to Neev's, which is the authoritative one.

    Reconciliation, not a user action. Neev owns the title: its resource row is
    what Projects lists and what its ACL is evaluated against, and the Docs copy
    exists so the editor shows the right name. They can only diverge when one of
    the two writes fails after the other succeeded — a rename that upstream
    rejected, or a native update that lost a revision conflict — and the repair
    is always the same direction: Neev's value wins.

    Idempotent, so the caller can send it after every divergence it notices
    without checking first. Returns `(document, changed)`.
    """
    document_uuid = parse_document_id(document_id)
    subject = require_subject(subject_value)
    service_user = get_service_principal()
    document = _engine_document_for(document_uuid, service_user, subject)

    new_title = clean_title(title)
    if document.title == new_title:
        return document, False

    document.title = new_title
    # `updated_at` is `auto_now`, so it is only refreshed when named here.
    document.save(update_fields=["title", "updated_at"])
    return document, True


def _engine_document_for(document_uuid, service_user, subject):
    """Fetch a document the bridge provisioned, for a subject in its org.

    Three refusals, each of which would otherwise be a way to reach a document
    from outside the organization that owns it:

    * unknown id — nothing to project onto;
    * a document the bridge did not provision (for example a preview-era row in
      a shared database) — not ours to grant access to;
    * a document belonging to another organization. Neev's proxy authorises
      every request already; this is the guard that stops a mistake there from
      being a cross-organization grant here.
    """
    return _engine_document_in_org(document_uuid, service_user, subject.org_id)


def _engine_document_in_org(document_uuid, service_user, org_id):
    """`_engine_document_for`, for a caller that names an org but no subject."""
    document = models.Document.objects.filter(pk=document_uuid).first()
    if document is None:
        raise BridgeError("unknown_document", status=404)
    if not _is_engine_document(document, service_user):
        raise BridgeError("not_an_engine_document", status=409)
    if document_org_id(document) != org_id:
        raise BridgeError("organization_mismatch", status=409)
    return document


# Codes the object store uses for "this key was never written". `NoSuchBucket`
# is deliberately absent: a missing bucket is a broken deployment, not an
# empty document.
MISSING_OBJECT_CODES = frozenset({"NoSuchKey", "404", "NotFound"})


def _is_missing_object(err):
    """True only when the stored object does not exist — a never-saved document."""
    if isinstance(err, FileNotFoundError):
        return True
    if isinstance(err, ClientError):
        return err.response.get("Error", {}).get("Code") in MISSING_OBJECT_CODES
    return False


def _convert_yjs_to_markdown(yjs_bytes):
    """Upstream's own converter: y-provider's `POST /api/convert/`.

    Imported per call so that an upstream rename is a refused conversion (503)
    rather than an import error that takes every bridge route down with it.
    Checked only when a conversion is actually needed: an empty or unchanged
    document does not depend on y-provider at all.
    """
    if not getattr(settings, "Y_PROVIDER_API_BASE_URL", None):
        # Upstream would build "None" + "convert/" and fail somewhere less clear.
        raise markdown_rules.ExtractionRefused(
            "converter_unavailable", 503, "converter_not_configured"
        )

    from core.services import mime_types
    from core.services.converter_services import YdocConverter

    return YdocConverter().convert(yjs_bytes, mime_types.YJS, mime_types.MARKDOWN)


def document_markdown(document_id, org_id, if_none_match):
    """The last saved text of an engine document, as markdown, for Neev Knowledge.

    Neev authorizes against its own ACL before calling; this is the second
    check, and it is narrower than any user's: an engine document, in the org
    the caller names, nothing else. It reads, it never writes, and it reads only
    what the editor saved (`PATCH content/`), not unsaved collaborative state.
    The extraction rules — caps, hash, fail-closed conversion and media
    redaction — are in `neev_bridge.markdown`.
    """
    document_uuid = parse_document_id(document_id)
    try:
        org = markdown_rules.require_org_id(org_id)
        tag = markdown_rules.require_tag(if_none_match)
    except markdown_rules.ExtractionRefused as err:
        raise BridgeError(err.code, err.status, err.detail) from err

    service_user = get_service_principal()
    document = _engine_document_in_org(document_uuid, service_user, org)
    if document.deleted_at or document.ancestors_deleted_at:
        # In Docs' trash: not a source, whatever Neev's ACL said.
        raise BridgeError("unknown_document", status=404)

    try:
        # Not `document.content`: upstream's property returns None for every
        # storage error, so an outage would read as an empty document.
        content = markdown_rules.read_stored_content(
            document.get_content_response, _is_missing_object
        )
        result = markdown_rules.extract(content, tag, _convert_yjs_to_markdown)
    except markdown_rules.ExtractionRefused as err:
        raise BridgeError(err.code, err.status, err.detail) from err

    result = {"id": str(document.id), **result}
    if not result["unchanged"]:
        result["updated_at"] = document.updated_at.isoformat()
    return result


@transaction.atomic
def project_access(document_id, subject_value, role, profile=None):
    """Make the Docs role for a subject match Neev's decision. Idempotent.

    Returns `(access, changed)`.
    """
    if role not in PROJECTABLE_ROLES:
        raise BridgeError("invalid_role")

    document_uuid = parse_document_id(document_id)
    subject = require_subject(subject_value)
    service_user = get_service_principal()

    document = _engine_document_for(document_uuid, service_user, subject)

    user = resolve_bridge_user(subject, profile)
    access, created = models.DocumentAccess.objects.get_or_create(
        document=document, user=user, defaults={"role": role}
    )
    changed = created
    if not created and access.role != role:
        # Never rewrite a privileged role: the service principal's ownership is
        # not something a projection call can downgrade.
        if access.role in (choices.RoleChoices.OWNER, choices.RoleChoices.ADMIN):
            raise BridgeError("privileged_access", status=409)
        access.role = role
        access.save(update_fields=["role"])
        changed = True

    if changed and not created:
        _reset_connections(document.id, user.id)
    return access, changed


@transaction.atomic
def revoke_access(document_id, subject_value):
    """Remove a projected access. Idempotent; never touches the principal."""
    document_uuid = parse_document_id(document_id)
    subject = require_subject(subject_value)
    service_user = get_service_principal()

    document = _engine_document_for(document_uuid, service_user, subject)

    user_model = service_user.__class__
    user = user_model.objects.filter(sub=subject.raw).first()
    if user is None or user.pk == service_user.pk:
        return False

    deleted, _ = (
        models.DocumentAccess.objects.filter(document=document, user=user)
        .exclude(role__in=(choices.RoleChoices.OWNER, choices.RoleChoices.ADMIN))
        .delete()
    )
    if deleted:
        _reset_connections(document.id, user.id)
    return bool(deleted)


def _reset_connections(document_id, user_id):
    """Ask the collaboration server to drop sockets holding stale rights.

    Scheduled with `on_commit`, never inline: these functions run inside
    `transaction.atomic`, and an HTTP call to y-provider from inside an open
    transaction would both hold the row locks for the duration of a network
    round trip and — worse — announce a role change that had not been committed
    yet, so y-provider could re-read the *old* role and cache it.

    Best effort even then: Neev's proxy closes the socket on its own
    authorization change, so this is a second line rather than the control.
    """

    def _notify():
        try:
            from core.services.collaboration_services import CollaborationService

            CollaborationService().reset_connections(str(document_id), str(user_id))
        except Exception:  # pragma: no cover - never fail a projection on this
            logger.warning(
                "neev-bridge: could not reset collaboration connections",
                exc_info=True,
            )

    transaction.on_commit(_notify)
