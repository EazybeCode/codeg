"""Run state and the target scope guard.

A run is bound to one tab and one origin. It starts `active` and leaves that
state exactly once, for good. Element references carry the snapshot epoch they
came from, so an action built from a stale reading of the page is refused
rather than applied to whatever now occupies that position.
"""

try:
    from urllib.parse import urlsplit
except ImportError:                                        # pragma: no cover
    from urlparse import urlsplit                          # type: ignore

ALLOWED_TOOLS = ("browser_snapshot", "browser_click", "browser_type",
                 "browser_scroll", "browser_screenshot",
                 "browser_request_user_action")

# Tools that address a specific element and therefore must quote a fresh ref.
REF_TOOLS = ("browser_click", "browser_type")

_DEFAULT_PORTS = {"http": "80", "https": "443"}
ALLOWED_SCHEMES = ("http", "https")


class ScopeError(Exception):
    def __init__(self, code, message):
        Exception.__init__(self, message)
        self.code = code


def normalize_origin(url):
    """Return the `scheme://host[:port]` origin of an http(s) URL.

    Anything that is not plain http(s), or that carries userinfo, is refused —
    a `https://app.test@evil.test/` style URL must never compare equal to the
    attached origin. IPv6 hosts are always bracketed so the textual form is
    stable.
    """
    parts = urlsplit((url or "").strip())
    scheme = (parts.scheme or "").lower()
    if scheme not in ALLOWED_SCHEMES:
        raise ScopeError("bad_origin", "scheme %r is not http(s)" % (scheme,))
    try:
        host = (parts.hostname or "").lower()
        port = parts.port
        userinfo = parts.username or parts.password
    except ValueError:
        raise ScopeError("bad_origin", "unparseable authority in %r" % (url,))
    if userinfo or "@" in (parts.netloc or ""):
        raise ScopeError("bad_origin", "URLs with userinfo are not accepted")
    if not host:
        raise ScopeError("bad_origin", "cannot derive an origin from %r" % (url,))
    if ":" in host:                                   # IPv6 literal
        host = "[%s]" % host
    if port is not None and str(port) != _DEFAULT_PORTS.get(scheme):
        return "%s://%s:%d" % (scheme, host, port)
    return "%s://%s" % (scheme, host)


def make_ref(epoch, index):
    return "e%d:%d" % (epoch, index)


def parse_ref(ref):
    """Return the epoch encoded in a ref, or raise ScopeError."""
    if not isinstance(ref, str) or not ref.startswith("e") or ":" not in ref:
        raise ScopeError("bad_ref", "malformed element reference %r" % (ref,))
    head, _, tail = ref[1:].partition(":")
    if not head.isdigit() or not tail.isdigit():
        raise ScopeError("bad_ref", "malformed element reference %r" % (ref,))
    return int(head)


class Target(object):
    def __init__(self, tab_id, origin):
        self.tab_id = tab_id
        self.origin = normalize_origin(origin)


class Run(object):
    def __init__(self, run_id, provider, target):
        self.run_id = run_id
        self.provider = provider
        self.target = target
        self.state = "active"
        self.stop_reason = None
        # The epoch counter is owned by the background service worker, which
        # survives navigation, so it is strictly increasing for the life of the
        # run. The host only ever accepts a larger one.
        self.epoch = 0
        self.refs_valid = False

    def _terminate(self, state, reason):
        if self.state != "active":
            return
        self.state = state
        self.stop_reason = reason

    def stop(self, reason):
        self._terminate("stopped", reason)

    def revoke(self, reason):
        self._terminate("revoked", reason)

    def observe_snapshot(self, epoch):
        """Record a fresh reading of the page and re-enable element refs."""
        if isinstance(epoch, bool) or not isinstance(epoch, int):
            raise ScopeError("bad_epoch", "epoch %r is not an integer" % (epoch,))
        if epoch <= self.epoch and self.refs_valid:
            raise ScopeError("bad_epoch",
                             "epoch %d did not advance past %d" % (epoch, self.epoch))
        if epoch < self.epoch:
            raise ScopeError("bad_epoch",
                             "epoch %d went backwards from %d" % (epoch, self.epoch))
        self.epoch = epoch
        self.refs_valid = True

    def invalidate_refs(self, reason):
        """A new document is loaded: every outstanding element ref is dead.

        This fires for same-origin navigation too. Without it, `e7:3` from the
        previous document would still parse as epoch 7 and could be applied to
        whatever now sits at that position.
        """
        self.refs_valid = False
        self.last_invalidation = reason

    def authorize(self, tool_name, tab_id, url, args=None):
        if self.state != "active":
            raise ScopeError("run_not_active", "run is %s" % self.state)
        if tool_name not in ALLOWED_TOOLS:
            raise ScopeError("tool_not_allowed", "%s is not allowlisted" % tool_name)
        if tab_id != self.target.tab_id or isinstance(tab_id, bool):
            raise ScopeError("wrong_tab", "tab %r is not the attached tab" % (tab_id,))
        if normalize_origin(url) != self.target.origin:
            raise ScopeError("wrong_origin",
                             "page left %s" % self.target.origin)
        if tool_name in REF_TOOLS:
            epoch = parse_ref((args or {}).get("ref"))
            if not self.refs_valid:
                raise ScopeError(
                    "stale_snapshot",
                    "the document changed since snapshot %d; "
                    "call browser_snapshot again" % epoch)
            if epoch != self.epoch:
                raise ScopeError(
                    "stale_snapshot",
                    "ref is from snapshot %d but the page is at %d; "
                    "call browser_snapshot again" % (epoch, self.epoch))
