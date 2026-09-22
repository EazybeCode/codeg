"""Upstream's forced-session middleware, skipped on the internal prefix.

Upstream creates a session for every unauthenticated request so anonymous
link-reach browsing works. Server-to-server bridge calls are neither anonymous
browsers nor session holders, and a session per provisioning call would be churn
in Redis for nobody's benefit.

Kept apart from `neev_bridge.middleware` because it imports `core`, which only
exists inside the Docs image (the middleware module itself stays importable
outside it, so the session rules can be tested without the image).
"""

from core.middleware import ForceSessionMiddleware

from neev_bridge.middleware import INTERNAL_PATH_PREFIX


class NeevForceSessionMiddleware(ForceSessionMiddleware):
    """`ForceSessionMiddleware`, minus the internal bridge paths."""

    def process_request(self, request):
        if request.path.startswith(INTERNAL_PATH_PREFIX):
            return None
        return super().process_request(request)
