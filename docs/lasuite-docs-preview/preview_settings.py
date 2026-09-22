"""Loopback-preview configuration: Production, minus the three TLS assumptions.

Subclasses upstream's Production so every dependency, app list and default it
relies on stays exactly as shipped. The Development configuration was the wrong
tool — it pulls in django_extensions, which is not installed in the production
image, so the container will not even start.

Only three settings change, and only because the preview is reached over an SSH
loopback tunnel on plain HTTP:

  SECURE_SSL_REDIRECT  Production redirects every request to https://, which
                       makes each API call a 301 the tunnel cannot follow.
  SESSION_COOKIE_SECURE / CSRF_COOKIE_SECURE
                       A browser will not store a Secure cookie received over
                       HTTP, so login cannot complete regardless of redirects.

Nothing else is relaxed: authentication, OIDC, permissions and the collaboration
socket all behave as they do in Production. This file is mounted read-only into
the preview container and exists nowhere else.
"""

from impress.settings import Production


class LoopbackPreview(Production):
    """Production for an HTTP loopback tunnel. Not for any exposed deployment."""

    SECURE_SSL_REDIRECT = False
    SESSION_COOKIE_SECURE = False
    CSRF_COOKIE_SECURE = False
