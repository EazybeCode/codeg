"""Pure helpers used to derive settings from upstream's own values.

Kept free of Django imports so the wiring rules can be tested without loading a
settings module, and so a mistake shows up as a failing assertion rather than as
a container that boots with the middleware in the wrong place.
"""

AUTH_MIDDLEWARE = "django.contrib.auth.middleware.AuthenticationMiddleware"
REMOTE_USER_MIDDLEWARE = "neev_bridge.middleware.NeevRemoteUserMiddleware"
THREAD_SCOPE_MIDDLEWARE = "neev_bridge.thread_scope.NeevThreadScopeMiddleware"
UPSTREAM_FORCE_SESSION = "core.middleware.ForceSessionMiddleware"
NEEV_FORCE_SESSION = "neev_bridge.force_session.NeevForceSessionMiddleware"
CSRF_MIDDLEWARE = "django.middleware.csrf.CsrfViewMiddleware"

REMOTE_USER_BACKEND = "neev_bridge.backends.NeevRemoteUserBackend"
MODEL_BACKEND = "django.contrib.auth.backends.ModelBackend"
OIDC_BACKEND = "core.authentication.backends.OIDCAuthenticationBackend"


def insert_after(entries, anchor, value):
    """Return `entries` with `value` placed directly after `anchor`."""
    result = list(entries)
    if value in result:
        return result
    try:
        index = result.index(anchor)
    except ValueError as err:
        raise ValueError(f"{anchor} missing from the middleware list") from err
    result.insert(index + 1, value)
    return result


def replace_entry(entries, old, new):
    """Return `entries` with `old` swapped for `new`, in place.

    Idempotent: a list that has already been rewritten comes back unchanged,
    because a configuration subclass re-deriving its parent's list must not be
    an error.
    """
    result = list(entries)
    if new in result and old not in result:
        return result
    try:
        index = result.index(old)
    except ValueError as err:
        raise ValueError(f"{old} missing from the middleware list") from err
    result[index] = new
    return result


def build_middleware(upstream):
    """Wire the bridge into upstream's middleware list.

    The remote-user middleware must sit *after* `AuthenticationMiddleware`
    (it needs `request.user`) and *before* `ForceSessionMiddleware`, so an
    authenticated request never picks up the anonymous session that upstream
    forces for everyone else. Upstream's own ordering already puts those two
    next to each other, so "immediately after" satisfies both.

    `CsrfViewMiddleware` stays exactly where upstream put it. Nothing here
    removes or reorders it.
    """
    wired = replace_entry(upstream, UPSTREAM_FORCE_SESSION, NEEV_FORCE_SESSION)
    wired = insert_after(wired, AUTH_MIDDLEWARE, REMOTE_USER_MIDDLEWARE)
    # After the identity is established, so a refusal is attributable in logs,
    # and before any view runs.
    return insert_after(wired, REMOTE_USER_MIDDLEWARE, THREAD_SCOPE_MIDDLEWARE)


def build_authentication_backends(upstream):
    """Bridge backend first, `ModelBackend` kept, OIDC removed.

    The OIDC backend is unreachable in this deployment (no OIDC endpoints are
    configured and no login route is exposed), so leaving it listed would only
    preserve a second way to mint an account.
    """
    backends = [REMOTE_USER_BACKEND]
    backends += [b for b in upstream if b not in (OIDC_BACKEND, REMOTE_USER_BACKEND)]
    return backends


# The two reads y-provider makes to authorise a collaboration socket, at
# v5.7.0 (`servers/y-provider/src/api/collaborationBackend.ts`). Matched by
# Django against the path without its leading slash. The document id is held to
# the UUID shape y-provider itself requires of a room, and both are anchored at
# the end, so nothing longer — `content/`, `threads/`, a list — is covered.
COLLABORATION_CALLBACKS = (
    r"^api/v1\.0/documents/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-"
    r"[0-9a-f]{4}-[0-9a-f]{12}/$",
    r"^api/v1\.0/users/me/$",
)


def build_redirect_exempt(upstream):
    """Upstream's HTTPS-redirect exemptions, plus y-provider's two callbacks.

    y-provider calls `COLLABORATION_BACKEND_BASE_URL` (`http://backend:8000`)
    directly on the internal network, with no `X-Forwarded-Proto`. Upstream's
    `Production` redirects every such request to `https://backend:8000`, a port
    that does not speak TLS, so every socket was refused as `Unauthorized`:
    no live collaboration, and a reconnect about every 35 seconds per open
    document. The loopback canary never saw it because it turns the redirect
    off altogether.

    Exempting only these paths keeps the redirect for everything else. Browser
    traffic reaches Django only through the Neev proxy, which always sends
    `X-Forwarded-Proto: https`, so for those requests nothing changes; the
    exemption only matters to a caller already inside the network.
    """
    exempt = list(upstream)
    exempt += [path for path in COLLABORATION_CALLBACKS if path not in exempt]
    return exempt


def check_wiring(middleware, backends):
    """Return a list of human-readable configuration problems (empty is good).

    Run at startup so a mis-ordered list fails loudly there rather than as a
    subtle authentication bug under load.
    """
    problems = []
    if REMOTE_USER_MIDDLEWARE not in middleware:
        problems.append("remote-user middleware is not installed")
    elif middleware.index(REMOTE_USER_MIDDLEWARE) < middleware.index(AUTH_MIDDLEWARE):
        problems.append("remote-user middleware runs before AuthenticationMiddleware")
    if CSRF_MIDDLEWARE not in middleware:
        problems.append("CsrfViewMiddleware has been removed")
    if THREAD_SCOPE_MIDDLEWARE not in middleware:
        problems.append("the thread-scope guard is not installed")
    if UPSTREAM_FORCE_SESSION in middleware:
        problems.append("upstream ForceSessionMiddleware was not replaced")
    if backends[:1] != [REMOTE_USER_BACKEND]:
        problems.append("the bridge backend is not first in AUTHENTICATION_BACKENDS")
    if OIDC_BACKEND in backends:
        problems.append("the OIDC backend is still enabled")
    return problems
