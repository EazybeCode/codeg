"""Root URL configuration: upstream's routes, minus admin, plus `/_neev/`.

Upstream's `impress.urls` is used as published; this module only

* drops the Django admin route, which has no place in a deployment whose only
  identity source is a proxy header, and
* adds the internal bridge endpoints.

Removing admin here is defence in depth, not the control: the proxy's allowlist
does not contain `/admin` either.
"""

from django.urls import path

from impress import urls as impress_urls

from neev_bridge import views

ADMIN_PREFIX = "admin/"


def upstream_patterns():
    """Upstream's patterns with the admin route removed."""
    return [
        pattern
        for pattern in impress_urls.urlpatterns
        if str(getattr(pattern, "pattern", "")) != ADMIN_PREFIX
    ]


urlpatterns = [
    path("_neev/v1/health", views.health, name="neev-bridge-health"),
    path(
        "_neev/v1/documents/provision",
        views.provision_document,
        name="neev-bridge-provision",
    ),
    path(
        "_neev/v1/documents/<str:document_id>/access",
        views.document_access,
        name="neev-bridge-access",
    ),
    path(
        "_neev/v1/documents/<str:document_id>/title",
        views.document_title,
        name="neev-bridge-title",
    ),
    path(
        "_neev/v1/sessions/mint",
        views.mint_session,
        name="neev-bridge-mint-session",
    ),
    path(
        "_neev/v1/sessions/resolve",
        views.resolve_session,
        name="neev-bridge-resolve-session",
    ),
    # Spliced in rather than `include()`d, so every upstream route name still
    # reverses exactly as upstream expects it to.
    *upstream_patterns(),
]
