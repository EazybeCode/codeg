"""Resolve a Docs session cookie to the subject it actually carries.

This exists because of an asymmetry between the two paths into Docs.

On HTTP, identity is asserted per request: the proxy strips whatever the client
sent and injects `X-Neev-Remote-User`, and the middleware repairs the session if
the cookie names someone else. On the **websocket** path there is no such step —
y-provider forwards only `cookie`, `origin` and `X-Y-Provider-Key` to the
backend, so Docs authorises the socket from `docs_sessionid` alone. A client that
holds a valid native Neev session as A and presents a Docs cookie belonging to B
would therefore open a socket as B, and no amount of correct ACL projection for A
would prevent it.

So before upgrading a websocket the proxy asks this endpoint whose session the
cookie actually is, and refuses unless the answer equals the subject it derived
natively. Cookie *presence* proves nothing; cookie *ownership* is what is
checked.

Deliberately core-free — it reads the configured session store and the user
model, both of which are Django's — so it can be tested outside the image.
"""

import re
from importlib import import_module

from django.conf import settings
from django.contrib.auth import (
    BACKEND_SESSION_KEY,
    HASH_SESSION_KEY,
    SESSION_KEY,
    get_user_model,
)

from neev_bridge.settings_support import REMOTE_USER_BACKEND

# Minted sessions are handshake credentials, not browsing sessions: short enough
# that a leaked one is worth little, long enough to outlive a collaboration
# callback. The proxy is expected to reuse a minted key until it nears expiry
# rather than mint one per upgrade.
DEFAULT_MINT_TTL_SECONDS = 3600
MAX_MINT_TTL_SECONDS = 12 * 3600

# Django session keys are 32 lowercase alphanumerics. Bounded and validated
# before use because the value reaches a cache key in the Redis-backed store.
SESSION_KEY_RE = re.compile(r"^[A-Za-z0-9]{8,64}$")

# Sessions minted by any other backend are not accepted: a session is only a
# Neev identity if the Neev backend established it.
ACCEPTED_BACKENDS = frozenset({REMOTE_USER_BACKEND})


def _session_store(session_key):
    engine = import_module(settings.SESSION_ENGINE)
    return engine.SessionStore(session_key)


def resolve_session(session_key):
    """Return the subject owning `session_key`, or None.

    None covers every failure mode — unknown, expired, anonymous, established by
    another backend, or belonging to a deactivated user — because the caller's
    only correct response to any of them is identical: refuse the upgrade.
    """
    if not isinstance(session_key, str) or not SESSION_KEY_RE.match(session_key):
        return None

    session = _session_store(session_key)
    # `SessionStore.load()` returns {} for an unknown or expired key.
    user_id = session.get("_auth_user_id")
    if not user_id:
        return None
    if session.get("_auth_user_backend") not in ACCEPTED_BACKENDS:
        return None

    user = get_user_model().objects.filter(pk=user_id).first()
    if user is None or not user.is_active or not user.sub:
        return None
    return user.sub


def mint_session(user, ttl_seconds=None):
    """Create a Docs session for `user` that no browser has ever held.

    This is the websocket path's answer to the problem `resolve_session`
    documents. Rather than trusting the cookie the client happens to present,
    the proxy asks for a session it derives itself, forwards *only* that one on
    the upgrade, and drops the client's. The client never influences which
    identity the socket gets, because it never supplies the cookie.

    The session is written exactly as `auth.login()` would write it — same keys,
    same session-auth hash — so upstream's own `get_user` accepts it and nothing
    here depends on session internals staying still beyond what Django
    documents.

    Returns `(session_key, ttl_seconds)`.
    """
    ttl = ttl_seconds or DEFAULT_MINT_TTL_SECONDS
    ttl = max(60, min(int(ttl), MAX_MINT_TTL_SECONDS))

    engine = import_module(settings.SESSION_ENGINE)
    session = engine.SessionStore()
    session[SESSION_KEY] = str(user.pk)
    session[BACKEND_SESSION_KEY] = REMOTE_USER_BACKEND
    session[HASH_SESSION_KEY] = user.get_session_auth_hash()
    session.set_expiry(ttl)
    session.create()
    return session.session_key, ttl
