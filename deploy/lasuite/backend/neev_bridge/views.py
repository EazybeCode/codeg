"""Internal bridge endpoints — server-to-server only.

These live under `/_neev/`, which the Neev proxy's route allowlist does not
contain, on a compose network that publishes no ports. They are additionally
gated by a shared credential compared in constant time. Three independent
conditions, none of which is "the caller said so".

They are not a second way into a document: they cannot grant a privileged
role and cannot touch a document the bridge did not create. One reads content —
`document_markdown`, the last saved text of an engine document in the org the
caller names, with media redacted — because Neev Knowledge needs it server-side
and Neev has already authorized the read against its own ACL.
They exist so that provisioning and ACL projection do not have to be performed
*as a user* over the browser API — which would mean either impersonating the
creator or carrying a session and CSRF token for a machine.

Because there is no browser and no cookie on this path, CSRF does not apply:
these views are session-less by construction (see `NeevRemoteUserMiddleware` and
`NeevForceSessionMiddleware`, both of which skip the prefix). Upstream's CSRF
protection on every browser-reachable route is untouched.
"""

import json
import logging

from django.conf import settings
from django.http import JsonResponse, StreamingHttpResponse
from django.utils.crypto import constant_time_compare
from django.views.decorators.csrf import csrf_exempt
from django.views.decorators.http import require_http_methods

from neev_bridge import service, session_lookup
from neev_bridge.backends import profile_from_request
from neev_bridge.middleware import SUBJECT_HEADER
from neev_bridge.identity import clean_profile_value

logger = logging.getLogger(__name__)

KEY_HEADER = "HTTP_X_NEEV_BRIDGE_KEY"
MIN_KEY_LENGTH = 32
MAX_BODY_BYTES = 16 * 1024


def _error(code, status, detail=None):
    payload = {"error": code}
    if detail:
        payload["detail"] = detail
    return JsonResponse(payload, status=status)


def _check_key(request):
    """Return an error response, or None when the caller is the bridge."""
    expected = getattr(settings, "NEEV_BRIDGE_KEY", None)
    if not expected or len(expected) < MIN_KEY_LENGTH:
        # Refuse rather than fall back to "no credential required".
        logger.error("neev-bridge: NEEV_BRIDGE_KEY is unset or too short")
        return _error("bridge_disabled", 503)
    presented = request.META.get(KEY_HEADER) or ""
    if not constant_time_compare(presented, expected):
        return _error("unauthorized", 401)
    return None


def _load_body(request):
    """Parse a bounded JSON object body. Raises `service.BridgeError`."""
    if len(request.body) > MAX_BODY_BYTES:
        raise service.BridgeError("body_too_large", status=413)
    if not request.body:
        return {}
    try:
        payload = json.loads(request.body.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as err:
        raise service.BridgeError("invalid_json") from err
    if not isinstance(payload, dict):
        raise service.BridgeError("invalid_json")
    return payload


def _profile(request, payload):
    """Advisory profile from headers, with body fields as a fallback."""
    profile = profile_from_request(request)
    email = clean_profile_value(payload.get("email"), 254)
    if "email" not in profile and email and "@" in email:
        profile["email"] = email
    name = clean_profile_value(payload.get("name"), 100)
    if "full_name" not in profile and name:
        profile["full_name"] = name
    return profile


def _bridge_view(handler):
    """Shared plumbing: credential, body bounds, refusal codes."""

    def wrapper(request, *args, **kwargs):
        denied = _check_key(request)
        if denied is not None:
            return denied
        try:
            return handler(request, *args, **kwargs)
        except service.BridgeError as err:
            return _error(err.code, err.status, err.detail)

    wrapper.__name__ = handler.__name__
    return wrapper


@csrf_exempt
@require_http_methods(["POST"])
@_bridge_view
def provision_document(request):
    """POST /_neev/v1/documents/provision"""
    payload = _load_body(request)
    document, created = service.provision_document(
        payload.get("id"),
        payload.get("title"),
        payload.get("sub"),
        _profile(request, payload),
    )
    return JsonResponse(
        {"id": str(document.id), "created": created}, status=201 if created else 200
    )


@csrf_exempt
@require_http_methods(["POST", "DELETE"])
@_bridge_view
def document_access(request, document_id):
    """POST/DELETE /_neev/v1/documents/<id>/access"""
    payload = _load_body(request)
    if request.method == "DELETE":
        revoked = service.revoke_access(document_id, payload.get("sub"))
        return JsonResponse({"revoked": revoked})

    access, changed = service.project_access(
        document_id,
        payload.get("sub"),
        payload.get("role"),
        _profile(request, payload),
    )
    return JsonResponse({"role": access.role, "changed": changed})


@csrf_exempt
@require_http_methods(["POST"])
@_bridge_view
def resolve_session(request):
    """POST /_neev/v1/sessions/resolve — whose session is this cookie?

    The proxy calls this before upgrading a websocket, and refuses the upgrade
    unless `sub` equals the subject it derived from the native session. See
    `neev_bridge.session_lookup` for why the websocket path needs it and the
    HTTP path does not.
    """
    payload = _load_body(request)
    subject = session_lookup.resolve_session(payload.get("session_key"))
    return JsonResponse({"sub": subject})


@csrf_exempt
@require_http_methods(["GET"])
@_bridge_view
def document_media(request, document_id):
    """GET /_neev/v1/documents/<id>/media?key=<key> — stream one attachment.

    The relay exists here rather than in the proxy because everything it needs
    is already here: the object-store client, the credentials, the network path
    to the bucket, and — the part that matters — upstream's own `get_abilities`,
    which decides the answer. Rebuilding that in the proxy would mean a second
    set of storage credentials, a second network route to the bucket, and a
    second implementation of a permission rule that already exists.

    The proxy still authorizes the public `/document-engine/media/<key>` request
    against Neev's ACL first. This endpoint is the *second* check, not the only
    one, and it is expressed in Docs' terms because Docs owns the attachment.

    No redirect and no signed URL is ever produced: the bytes are read here and
    relayed. There is nothing shareable to leak.
    """
    subject = request.META.get(SUBJECT_HEADER)
    media = service.open_media(document_id, request.GET.get("key"), subject)

    response = StreamingHttpResponse(
        media.chunks(), content_type=media.content_type, status=200
    )
    disposition = "inline" if media.inline else "attachment"
    # The filename is the stored key's own, because the uploaded name does not
    # survive: upstream's malware-detection callback rewrites the object's
    # metadata and drops the Content-Disposition it had set.
    response["Content-Disposition"] = (
        f'{disposition}; filename="{escape_filename(media.filename)}"'
    )
    response["Content-Length"] = str(media.length)
    # Nothing here is a document that may run anything, be framed, or be sniffed
    # into something more dangerous than it was stored as.
    response["X-Content-Type-Options"] = "nosniff"
    response["Content-Security-Policy"] = "default-src 'none'; sandbox"
    response["Cache-Control"] = "private, no-store"
    # Deliberately absent: every `x-amz-*` header the object store returns.
    # `x-amz-meta-owner` is a Docs user id, and none of them are the browser's
    # business.
    return response


def escape_filename(name):
    """A quoted-string filename with nothing that could end the quoting."""
    cleaned = "".join(ch for ch in name if ch.isprintable() and ch not in '"\\;\r\n')
    return cleaned[:120] or "attachment"


@csrf_exempt
@require_http_methods(["POST"])
@_bridge_view
def document_title(request, document_id):
    """POST /_neev/v1/documents/<id>/title — make the Docs title match Neev's.

    For reconciliation after a divergence: a rename upstream rejected, or a
    native update that lost a revision conflict. Neev's value always wins,
    because Neev's resource row is the one its ACL and its Projects list are
    built from.
    """
    payload = _load_body(request)
    document, changed = service.set_title(
        document_id, payload.get("title"), payload.get("sub")
    )
    return JsonResponse({"title": document.title, "changed": changed})


@csrf_exempt
@require_http_methods(["POST"])
@_bridge_view
def document_markdown(request, document_id):
    """POST /_neev/v1/documents/<id>/markdown — saved text for Neev Knowledge.

    Neev authorizes against its own ACL first; this endpoint is private,
    credentialled, never in the proxy's allowlist, and bound to the org the
    caller names. It is the one bridge route that returns document text, so the
    response is never cached and carries no media URL (see
    `neev_bridge.markdown`).
    """
    payload = _load_body(request)
    response = JsonResponse(
        service.document_markdown(
            document_id, payload.get("org_id"), payload.get("if_none_match")
        )
    )
    response["Cache-Control"] = "private, no-store"
    return response


@csrf_exempt
@require_http_methods(["POST"])
@_bridge_view
def mint_session(request):
    """POST /_neev/v1/sessions/mint — a session the client never supplied.

    Used for the websocket upgrade: the proxy forwards the cookie it got here
    and drops whatever the browser sent, so the socket's identity is the one
    Neev derived rather than the one the client presented.
    """
    payload = _load_body(request)
    subject = service.require_subject(payload.get("sub"))
    user = service.resolve_bridge_user(subject, _profile(request, payload))
    session_key, ttl = session_lookup.mint_session(user, payload.get("ttl_seconds"))
    return JsonResponse(
        {
            "cookie_name": settings.SESSION_COOKIE_NAME,
            "session_key": session_key,
            "expires_in": ttl,
        }
    )


@csrf_exempt
@require_http_methods(["GET"])
@_bridge_view
def health(request):
    """GET /_neev/v1/health — credentialled liveness for the proxy's own checks."""
    return JsonResponse({"status": "ok"})
