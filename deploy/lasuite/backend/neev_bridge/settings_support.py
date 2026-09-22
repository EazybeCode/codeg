"""Pure helpers used to derive settings from upstream's own values.

Kept free of Django imports so the wiring rules can be tested without loading a
settings module, and so a mistake shows up as a failing assertion rather than as
a container that boots with the middleware in the wrong place.
"""

AUTH_MIDDLEWARE = "django.contrib.auth.middleware.AuthenticationMiddleware"
REMOTE_USER_MIDDLEWARE = "neev_bridge.middleware.NeevRemoteUserMiddleware"
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
    return insert_after(wired, AUTH_MIDDLEWARE, REMOTE_USER_MIDDLEWARE)


def build_authentication_backends(upstream):
    """Bridge backend first, `ModelBackend` kept, OIDC removed.

    The OIDC backend is unreachable in this deployment (no OIDC endpoints are
    configured and no login route is exposed), so leaving it listed would only
    preserve a second way to mint an account.
    """
    backends = [REMOTE_USER_BACKEND]
    backends += [b for b in upstream if b not in (OIDC_BACKEND, REMOTE_USER_BACKEND)]
    return backends


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
    if UPSTREAM_FORCE_SESSION in middleware:
        problems.append("upstream ForceSessionMiddleware was not replaced")
    if backends[:1] != [REMOTE_USER_BACKEND]:
        problems.append("the bridge backend is not first in AUTHENTICATION_BACKENDS")
    if OIDC_BACKEND in backends:
        problems.append("the OIDC backend is still enabled")
    return problems
