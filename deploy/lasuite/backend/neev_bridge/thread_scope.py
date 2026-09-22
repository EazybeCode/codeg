"""Make a nested thread route mean what its URL says.

Upstream scopes comment **reads** to the document in the URL —
`CommentViewSet.get_queryset` filters `thread__document=resource_id` — but the
**create** path does not go through that queryset. `CommentPermission` checks
`comment` on the document named in the URL, and `CommentSerializer.validate`
takes `thread_id` from the URL without checking that the thread belongs to it.

So a caller who may comment on *any* document can write into *any* thread whose
id they know:

    POST /api/v1.0/documents/<a-document-they-can-comment-on>/threads/<someone-else's-thread>/comments/

Measured on the canary against v5.7.0: a user with no access to the target
document — 403 on every one of its own routes — posted a comment through their
own document's URL, got 201, and the text appeared in the private thread for its
owner to read. Reads stayed correctly scoped, which is what makes it easy to
miss.

This middleware makes the pairing a precondition of the request: if the thread in
the path does not belong to the document in the path, the route does not exist.
One indexed point read, only on nested thread routes, and it closes the write
path, the reaction path and anything else nested under a thread at once —
without forking the image or waiting for an upstream fix.
"""

import re

from django.core.exceptions import ValidationError
from django.http import JsonResponse
from django.utils.deprecation import MiddlewareMixin

# Upstream's own URL pattern (`core/urls.py`) matches `[0-9a-z-]*` for both ids,
# so this matches the same shape rather than a stricter one — a request upstream
# would route must be a request this inspects.
THREAD_ROUTE = re.compile(
    r"^/api/v[\d.]+/documents/(?P<document>[0-9a-z-]+)/threads/(?P<thread>[0-9a-z-]+)(?:/|$)"
)


def scoped_pair(path):
    """Return `(document_id, thread_id)` for a nested thread route, else None."""
    match = THREAD_ROUTE.match(path or "")
    if match is None:
        return None
    return match.group("document"), match.group("thread")


class NeevThreadScopeMiddleware(MiddlewareMixin):
    """Refuse a thread route whose thread is not in its document."""

    def process_request(self, request):
        pair = scoped_pair(request.path)
        if pair is None:
            return None
        document_id, thread_id = pair

        # Imported here so the module stays importable outside the Docs image.
        from core.models import Thread

        try:
            exists = Thread.objects.filter(
                pk=thread_id, document_id=document_id
            ).exists()
        except (ValueError, TypeError, ValidationError):
            # Malformed ids are upstream's to answer; this middleware only
            # decides the pairing, and an id that cannot be a key has no pairing.
            return None

        if exists:
            return None
        # Same shape and status upstream uses for a thread it cannot find, so a
        # mismatched pair is indistinguishable from one that does not exist.
        return JsonResponse({"detail": "Not found."}, status=404)
