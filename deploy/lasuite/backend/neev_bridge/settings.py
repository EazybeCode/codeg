"""Neev configuration for the stock `lasuite/impress-backend:v5.7.0` image.

Mounted, not built in: the image is used exactly as published and this package is
bind-mounted at `/app/neev_bridge`, the same seam the preview already proved with
`preview_settings.py`. No image fork, no code fork, no patched upstream file.

`NeevProduction` subclasses upstream's `Production`, so every default, app and
dependency it relies on stays as shipped. What changes is only what has to:

* identity arrives from the Neev proxy in a header instead of from OIDC;
* cookies are scoped to the `/document-engine/` prefix the proxy serves under;
* the OIDC backend, Swagger, Silk and the admin route are switched off, because
  anything reachable that is not needed is only a way in;
* an internal, credentialled, session-less provisioning path is added under
  `/_neev/`, which no browser route reaches.

What deliberately does *not* change:

* `SESSION_COOKIE_NAME` stays `docs_sessionid` — y-provider looks for exactly
  that string when it authorises a collaboration socket;
* CSRF protection stays on, with upstream's middleware in upstream's position;
* `MEDIA_BASE_URL` stays unset, so the API keeps returning root-relative
  `/media/<key>` and the frontend's `backendUrl()` — which already carries
  `/document-engine` — composes the one correct prefix rather than two;
* `COLLABORATION_WS_URL` stays unset, so the frontend derives the socket URL
  same-origin instead of having a host baked into the bundle.
"""

import os
import sys

from django.core.exceptions import ImproperlyConfigured

from configurations import values

from impress.settings import Production

from neev_bridge.settings_support import (
    COLLABORATION_CALLBACKS,
    build_authentication_backends,
    build_middleware,
    build_redirect_exempt,
    check_wiring,
)

# The prefix the Neev proxy serves Docs under. Cookies are scoped to it so a
# Docs session cookie is never presented to Neev's own routes.
ENGINE_PATH_PREFIX = "/document-engine/"


class NeevProduction(Production):
    """Production, with Neev as the only entry point."""

    ROOT_URLCONF = "neev_bridge.urls"

    MIDDLEWARE = build_middleware(Production.MIDDLEWARE)
    AUTHENTICATION_BACKENDS = build_authentication_backends(
        Production.AUTHENTICATION_BACKENDS
    )

    # Shared credential for the internal provisioning/projection endpoints.
    # Absent or short: those endpoints refuse every call (see views._check_key).
    NEEV_BRIDGE_KEY = values.Value(
        None, environ_name="NEEV_BRIDGE_KEY", environ_prefix=None
    )

    # Cookies. The name is upstream's and must not change; the scope is ours.
    SESSION_COOKIE_PATH = ENGINE_PATH_PREFIX
    CSRF_COOKIE_PATH = ENGINE_PATH_PREFIX
    SESSION_COOKIE_SAMESITE = "Lax"
    CSRF_COOKIE_SAMESITE = "Lax"
    SESSION_COOKIE_SECURE = True
    CSRF_COOKIE_SECURE = True

    # y-provider authorises a socket over plain HTTP inside the network; the
    # redirect upstream applies to it pointed at a port with no TLS and refused
    # every collaboration connection. See `build_redirect_exempt`.
    SECURE_REDIRECT_EXEMPT = build_redirect_exempt(Production.SECURE_REDIRECT_EXEMPT)

    # Nothing here is meant to be reachable without Neev.
    USE_SWAGGER = False
    SILK_ENABLED = False
    OIDC_RESOURCE_SERVER_ENABLED = False

    @classmethod
    def post_setup(cls):
        """Upstream's own post-setup, then refuse to start if mis-wired."""
        super().post_setup()

        problems = check_wiring(cls.MIDDLEWARE, cls.AUTHENTICATION_BACKENDS)

        if cls.SESSION_COOKIE_NAME != "docs_sessionid":
            problems.append(
                "SESSION_COOKIE_NAME must stay 'docs_sessionid' for y-provider"
            )
        if getattr(cls, "MEDIA_BASE_URL", None):
            problems.append(
                "MEDIA_BASE_URL must stay unset; the frontend already prefixes media"
            )
        if getattr(cls, "COLLABORATION_WS_URL", None):
            problems.append(
                "COLLABORATION_WS_URL must stay unset; the frontend derives it"
            )
        if cls.SECURE_SSL_REDIRECT and not set(COLLABORATION_CALLBACKS) <= set(
            cls.SECURE_REDIRECT_EXEMPT
        ):
            problems.append(
                "y-provider's callbacks must be exempt from the HTTPS redirect"
            )
        if cls.requires_secure_cookies() and not (
            cls.SESSION_COOKIE_SECURE and cls.CSRF_COOKIE_SECURE
        ):
            problems.append("session and CSRF cookies must be Secure")

        if problems:
            message = "Neev bridge configuration is unsafe: " + "; ".join(problems)
            # Printed as well as raised: Django's management utility swallows an
            # exception raised during settings setup and later fails with
            # "Apps aren't loaded yet", which tells an operator nothing. The
            # process still refuses to start — this only makes it say why.
            print(f"NEEV BRIDGE REFUSED TO START: {message}", file=sys.stderr, flush=True)
            raise ImproperlyConfigured(message)

    @classmethod
    def requires_secure_cookies(cls):
        """Production always does. The canary below is the one exception."""
        return True


class NeevCanary(NeevProduction):
    """Loopback canary only — never an exposed deployment.

    The native canary is reached over plain HTTP on `http://127.0.0.1:3100`, and
    a browser will not store a `Secure` cookie received over HTTP. Relaxing that
    is the single difference from `NeevProduction`, it is opt-in via
    `NEEV_CANARY=1`, and it refuses to start without that acknowledgement so the
    configuration cannot be selected for production by a copied env file.
    """

    SESSION_COOKIE_SECURE = False
    CSRF_COOKIE_SECURE = False
    SECURE_SSL_REDIRECT = False
    SECURE_HSTS_SECONDS = 0

    @classmethod
    def requires_secure_cookies(cls):
        return False

    @classmethod
    def post_setup(cls):
        if os.environ.get("NEEV_CANARY") != "1":
            print(
                "NEEV BRIDGE REFUSED TO START: NeevCanary without NEEV_CANARY=1",
                file=sys.stderr,
                flush=True,
            )
            raise ImproperlyConfigured(
                "NeevCanary refuses to start without NEEV_CANARY=1. "
                "It disables Secure cookies and is for a loopback canary only."
            )
        super().post_setup()
