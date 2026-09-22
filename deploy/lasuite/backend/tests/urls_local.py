"""Probe views for the local tests.

`/whoami` reports what the middleware chain decided: which subject is
authenticated, and which session key carries it. Session identity is the thing
under test, so it is reported rather than inferred from cookies alone.
"""

from django.http import JsonResponse
from django.urls import path


def whoami(request):
    """Report the authenticated subject and the live session key."""
    user = request.user
    return JsonResponse(
        {
            "authenticated": user.is_authenticated,
            "sub": getattr(user, "sub", None),
            "email": getattr(user, "email", None),
            "full_name": getattr(user, "full_name", None),
            "short_name": getattr(user, "short_name", None),
            "session_key": request.session.session_key,
            "csrf_cookie": request.META.get("CSRF_COOKIE"),
        }
    )


urlpatterns = [path("whoami", whoami, name="whoami")]
