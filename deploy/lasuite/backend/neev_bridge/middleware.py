"""Remote-user middleware for the Neev bridge.

Why this exists rather than `django.contrib.auth.middleware.PersistentRemoteUserMiddleware`
used as shipped:

`RemoteUserMiddleware.process_request` keeps an existing session only when

    request.user.get_username() == self.clean_username(header_value)

`get_username()` returns `USERNAME_FIELD`, which for `core.models.User` is
`admin_email` — an OIDC-era field that is NULL for every Neev-provisioned user.
It can never equal `neev:<org>:<user>`, so the stock middleware would take the
mismatch branch on *every* request: logout, re-authenticate, `auth.login()`.
`login()` cycles the session key and `rotate_token()` issues a new CSRF token, so
each request would hand the browser a different `docs_sessionid` and `csrftoken`.
Parallel requests would then race, and the collaboration socket — which is
authorised from the cookie by y-provider — would be authenticating with a session
that no longer exists by the time it connects.

The comparison here is therefore against `User.sub`, the field the header
actually names. The persistent behaviour (no header → keep the session) is kept,
because y-provider forwards only `cookie`, `origin` and `X-Y-Provider-Key` to the
backend: the identity header is absent by design on every collaboration callback.
"""

import logging

from django.contrib import auth
from django.contrib.auth.middleware import PersistentRemoteUserMiddleware

from neev_bridge.backends import profile_from_request, refresh_profile
from neev_bridge.identity import parse_browser_subject

logger = logging.getLogger(__name__)

SUBJECT_HEADER = "HTTP_X_NEEV_REMOTE_USER"

# Server-to-server bridge endpoints authenticate themselves and must never
# acquire, rotate or consume a browser session.
INTERNAL_PATH_PREFIX = "/_neev/"


class NeevRemoteUserMiddleware(PersistentRemoteUserMiddleware):
    """Persistent remote-user middleware that compares on `sub`."""

    header = SUBJECT_HEADER
    force_logout_if_no_header = False

    def process_request(self, request):
        """Authenticate the asserted subject, reusing the session when it matches."""
        if request.path.startswith(INTERNAL_PATH_PREFIX):
            return None

        raw = request.META.get(self.header)
        if raw is None:
            # Persistent: the collaboration callback legitimately has no header.
            return None

        subject = parse_browser_subject(raw)
        if subject is None:
            # Malformed, or an attempt to assert the reserved service namespace.
            # Fail closed, and do not let a previous identity survive a bad one.
            logger.warning("neev-bridge: refused remote subject on the header path")
            self._drop_session(request)
            return None

        if request.user.is_authenticated:
            if getattr(request.user, "sub", None) == subject.raw:
                # Same principal: keep the session, the session key and the CSRF
                # token exactly as they are.
                self._refresh_profile(request)
                return None
            # A different principal on an existing session — an organization
            # switch, or a reused cookie. Drop it before authenticating.
            self._drop_session(request)

        user = auth.authenticate(request, remote_user=subject.raw)
        if user:
            request.user = user
            auth.login(request, user)
        return None

    def _drop_session(self, request):
        """Log out unconditionally, whatever backend established the session.

        Not `_remove_invalid_user`: that only logs out when the session was
        established by a `RemoteUserBackend`, so a session created some other way
        (`ModelBackend`, or a leftover from an earlier configuration) would
        survive a refused assertion and keep answering as its old identity.
        """
        if request.user.is_authenticated:
            auth.logout(request)

    def _refresh_profile(self, request):
        """Keep advisory email/name current without re-authenticating."""
        profile = profile_from_request(request)
        if not profile:
            return
        try:
            refresh_profile(request.user, profile)
        except Exception:  # pragma: no cover - never fail a request on profile data
            logger.exception("neev-bridge: could not refresh profile fields")
