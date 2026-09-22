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
import uuid

from django.core.exceptions import ValidationError as DjangoValidationError
from django.db import IntegrityError, transaction

from core import choices, models
from core.utils.treebeard import create_tree_node_with_retry

from neev_bridge.identity import SERVICE_SUBJECT, Subject, parse_subject

logger = logging.getLogger(__name__)

# Roles the bridge is allowed to project. Everything else is refused, including
# `owner`, `administrator` and `commenter`.
PROJECTABLE_ROLES = (choices.RoleChoices.READER, choices.RoleChoices.EDITOR)


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
    document = models.Document.objects.filter(pk=document_uuid).first()
    if document is None:
        raise BridgeError("unknown_document", status=404)
    if not _is_engine_document(document, service_user):
        raise BridgeError("not_an_engine_document", status=409)
    if document_org_id(document) != subject.org_id:
        raise BridgeError("organization_mismatch", status=409)
    return document


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
