"""Neev remote identity: parsing and validation.

Pure Python — no Django import — so the rules can be unit-tested on their own and
read without a settings module loaded.

The proxy authenticates the Neev session and then asserts identity in a header:

    X-Neev-Remote-User: neev:<org_id>:<user_id>

That string becomes `core.models.User.sub` in Docs. It is the *only* identity key.
`X-Neev-Remote-Email` and `X-Neev-Remote-Name` are advisory profile data: they are
written onto the user record but never resolve, match or create one.

Two rules in here are load-bearing rather than cosmetic:

* the id segments may not contain `:`, so `neev:1:23` and `neev:12:3` cannot be
  made to name the same principal by moving the separator;
* the `neev:service:*` namespace is reserved for server-to-server principals and
  is refused on the header path (see `is_reserved_subject`). A browser-borne
  identity therefore cannot become the principal that owns every document, even
  if header stripping upstream were bypassed.
"""

import re

PREFIX = "neev"

# Deliberately permissive about the *shape* of an id (Neev ids may be integers or
# UUIDs) and strict about its alphabet: no colon (separator confusion), no
# whitespace, no non-ASCII (`core.validators.sub_validator` rejects non-ASCII at
# the model layer, and we would rather fail before the database does).
_SEGMENT = r"[0-9A-Za-z_-]{1,64}"
_SUBJECT_RE = re.compile(rf"^{PREFIX}:({_SEGMENT}):({_SEGMENT})$")

# `core.models.User.sub` is CharField(max_length=255).
MAX_SUBJECT_LENGTH = 255

# Reserved organization segment for non-human principals.
SERVICE_ORG = "service"

# The single server-to-server principal: owner of every engine-provisioned
# document, and the actor for access projection. Never a browser identity.
SERVICE_SUBJECT = f"{PREFIX}:{SERVICE_ORG}:engine"


class Subject:
    """A parsed, validated remote subject."""

    __slots__ = ("raw", "org_id", "user_id")

    def __init__(self, raw, org_id, user_id):
        self.raw = raw
        self.org_id = org_id
        self.user_id = user_id

    @property
    def is_service(self):
        """True for the reserved server-to-server namespace."""
        return self.org_id == SERVICE_ORG

    def __eq__(self, other):
        return isinstance(other, Subject) and other.raw == self.raw

    def __hash__(self):
        return hash(self.raw)

    def __repr__(self):
        return f"Subject({self.raw!r})"


def parse_subject(value):
    """Parse a remote-user header value. Return a `Subject`, or None.

    Fails closed: None means "no identity", never "some default identity".
    """
    if not isinstance(value, str):
        return None
    if not value or len(value) > MAX_SUBJECT_LENGTH:
        return None
    # A header value that needed stripping is a malformed one; do not repair it.
    if value != value.strip():
        return None
    match = _SUBJECT_RE.match(value)
    if match is None:
        return None
    return Subject(value, match.group(1), match.group(2))


def is_reserved_subject(value):
    """True when the value names a service principal, parsed or not.

    Checked as a string so a malformed-but-service-looking value is still
    recognised as reserved rather than slipping through as "just invalid".
    """
    return isinstance(value, str) and value.startswith(f"{PREFIX}:{SERVICE_ORG}:")


def parse_browser_subject(value):
    """Parse a subject that arrived on the browser (header) path.

    Same as `parse_subject`, minus the reserved namespace.
    """
    if is_reserved_subject(value):
        return None
    return parse_subject(value)


def clean_profile_value(value, max_length):
    """Normalise an advisory profile header (email, display name).

    Advisory data is attacker-influenced in the sense that it is whatever Neev
    holds for the account, so it is bounded and stripped of control characters
    before it reaches the database. Returns None when there is nothing usable.
    """
    if not isinstance(value, str):
        return None
    cleaned = "".join(ch for ch in value if ch.isprintable()).strip()
    if not cleaned:
        return None
    return cleaned[:max_length]
