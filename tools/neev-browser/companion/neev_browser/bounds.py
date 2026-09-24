"""Bound and redact page snapshots before any of it reaches the model.

This is an allowlist, not a denylist: a key the content script starts sending
tomorrow is dropped here unless it is named below. Field values never survive.
"""

import json

MAX_NODES = 200
MAX_NAME_CHARS = 120
MAX_SNAPSHOT_BYTES = 49152
REDACTED_ROLE = "redacted"

_SNAPSHOT_KEYS = ("url", "title")
_NODE_KEYS = ("ref", "role", "tag", "name", "placeholder", "enabled",
              "filled", "checked", "consequential")
_REDACTED_ROLES = ("password", "hidden")
# URLs can be long and are not names; they get their own, looser budget.
_MAX_URL_CHARS = 512


def _clip(value, limit):
    return value[:limit] if isinstance(value, str) else value


def _bound_node(raw):
    if not isinstance(raw, dict):
        return {"role": REDACTED_ROLE}
    if raw.get("role") in _REDACTED_ROLES:
        return {"ref": raw.get("ref"), "role": REDACTED_ROLE}
    out = {}
    for key in _NODE_KEYS:
        if key in raw:
            out[key] = _clip(raw[key], MAX_NAME_CHARS)
    return out


def bound_snapshot(raw):
    if not isinstance(raw, dict):
        return {"truncated": False, "nodes": []}
    nodes = raw.get("nodes")
    nodes = list(nodes) if isinstance(nodes, list) else []
    # The extension clips to MAX_NODES before sending, so counting here would
    # always say "nothing was dropped" — the flag has to survive the boundary
    # or the model is told it read the whole page when it read 200 of 5000.
    # `is True` rather than truthiness: this arrives as JSON from elsewhere.
    out = {"truncated": raw.get("truncated") is True or len(nodes) > MAX_NODES,
           "nodes": [_bound_node(node) for node in nodes[:MAX_NODES]]}
    for key in _SNAPSHOT_KEYS:
        if key in raw:
            limit = _MAX_URL_CHARS if key == "url" else MAX_NAME_CHARS
            out[key] = _clip(raw[key], limit)
    while (len(json.dumps(out).encode("utf-8")) > MAX_SNAPSHOT_BYTES
           and out["nodes"]):
        out["nodes"].pop()
        out["truncated"] = True
    return out
