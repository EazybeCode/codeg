#!/usr/bin/env python3
"""Build the Microsoft Teams app package (manifest.json + icons, zipped) for Neev.

The package is what a Teams administrator uploads in the Teams admin center
(Manage apps -> Upload new app). It declares the single-tenant Azure Bot the
Neev server is configured with (NEEV_TEAMS_BOT_APP_ID) in personal and team
scope, and -- only when asked -- the tab SSO resource (NEEV_TEAMS_TAB_APP_ID_URI)
and a personal static tab.

Requires jsonschema for the vendored MicrosoftTeams.v1.29 schema and Pillow
for PNG validation. Install with:

    python3 -m pip install -r scripts/teams/requirements.txt

    python3 scripts/teams/build_app_package.py \
        --bot-app-id 11111111-2222-3333-4444-555555555555 \
        --base-url https://neev.example.com \
        --output dist/neev-teams.zip
"""

from __future__ import annotations

import argparse
import io
import ipaddress
import json
import os
import re
import struct
import sys
import zipfile
import zlib
from pathlib import Path
from urllib.parse import urlsplit

MANIFEST_VERSION = "1.29"
SCHEMA_URL = (
    "https://developer.microsoft.com/en-us/json-schemas/teams/v1.29/"
    "MicrosoftTeams.schema.json"
)
SCHEMA_PATH = Path(__file__).with_name("MicrosoftTeams.v1.29.schema.json")
GUID = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)
SEMVER = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
ACCENT = "#1F6FEB"
# A zip entry's fixed timestamp keeps the package byte-for-byte reproducible.
ZIP_TIME = (2026, 1, 1, 0, 0, 0)


class PackageError(ValueError):
    """A refused input; the message names the flag to fix."""


def _https_url(url: str, flag: str, *, origin: bool = False) -> str:
    try:
        parts = urlsplit(url)
        parts.port  # Reject malformed/out-of-range ports before producing a package.
    except ValueError as error:
        raise PackageError(f"{flag} must be a valid HTTPS URL") from error
    if (
        parts.scheme != "https" or not parts.hostname
        or parts.username is not None or parts.password is not None
        or "?" in url or "#" in url or any(c.isspace() for c in url)
        or (origin and parts.path not in ("", "/"))
    ):
        kind = "origin" if origin else "URL"
        raise PackageError(
            f"{flag} must be an https:// {kind} without credentials, query, or fragment"
        )
    # urlsplit separates components but does not validate a DNS authority.
    # Percent escapes and backslashes can change how clients interpret hosts.
    try:
        host = parts.hostname
        if "\\" in url or "%" in parts.netloc or any(ord(c) < 32 for c in url):
            raise ValueError("invalid authority character")
        if parts.netloc.startswith("["):
            ipaddress.IPv6Address(host)
            suffix = parts.netloc[parts.netloc.index("]") + 1:]
            if suffix and not re.fullmatch(r":[0-9]+", suffix):
                raise ValueError("invalid IPv6 port suffix")
        else:
            if parts.netloc.endswith(":"):
                raise ValueError("empty port")
            ascii_host = host.encode("idna").decode("ascii").rstrip(".")
            if re.fullmatch(r"[0-9.]+", ascii_host):
                ipaddress.IPv4Address(ascii_host)
            elif len(ascii_host) > 253 or not all(
                re.fullmatch(r"[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?", label)
                for label in ascii_host.split(".")
            ):
                raise ValueError("invalid DNS name")
    except (ValueError, UnicodeError) as error:
        raise PackageError(f"{flag} must have a valid DNS or IP host and port") from error
    return url.rstrip("/")


def _guid(value: str, flag: str) -> str:
    if not GUID.match(value or ""):
        raise PackageError(f"{flag} must be a GUID")
    return value.lower()


def build_manifest(
    *,
    bot_app_id: str,
    base_url: str,
    app_id: str | None = None,
    version: str = "1.0.0",
    developer_name: str = "Neev",
    privacy_url: str | None = None,
    terms_url: str | None = None,
    tab_app_id_uri: str | None = None,
    tab_content_url: str | None = None,
) -> dict:
    bot_app_id = _guid(bot_app_id, "--bot-app-id")
    app_id = _guid(app_id, "--app-id") if app_id else bot_app_id
    base = _https_url(base_url, "--base-url", origin=True)
    if not SEMVER.match(version):
        raise PackageError("--version must be MAJOR.MINOR.PATCH")
    if not developer_name or len(developer_name) > 32:
        raise PackageError("--developer-name must be 1-32 characters")
    host = urlsplit(base).hostname
    manifest: dict = {
        "$schema": SCHEMA_URL,
        "manifestVersion": MANIFEST_VERSION,
        "version": version,
        "id": app_id,
        "developer": {
            "name": developer_name,
            "websiteUrl": base,
            "privacyUrl": _https_url(privacy_url, "--privacy-url") if privacy_url else f"{base}/privacy",
            "termsOfUseUrl": _https_url(terms_url, "--terms-url") if terms_url else f"{base}/terms",
        },
        "name": {"short": "Neev", "full": "Neev agent workspace"},
        "description": {
            "short": "Run your Neev agent chats from Teams.",
            "full": (
                "Message Neev directly, or mention @Neev in a connected channel, to run "
                "your request as a Neev agent chat in the project your administrator "
                "connected. Only Microsoft 365 users your Neev administrator has linked "
                "to a Neev account can use it; everyone else is told so."
            ),
        },
        "icons": {"color": "color.png", "outline": "outline.png"},
        "accentColor": ACCENT,
        "bots": [
            {
                "botId": bot_app_id,
                # Group chats are refused by the server (`teams_group_chat`), so
                # the app does not offer them.
                "scopes": ["personal", "team"],
                "isNotificationOnly": False,
                "supportsFiles": False,
                "commandLists": [
                    {
                        "scopes": ["personal", "team"],
                        "commands": [
                            {
                                "title": "Ask",
                                "description": "Send a request to your Neev agent",
                            }
                        ],
                    }
                ],
            }
        ],
        "permissions": ["identity"],
        "validDomains": [host],
    }
    if tab_app_id_uri:
        if not tab_app_id_uri.startswith("api://") or not (
            tab_app_id_uri.lower().endswith(f"/{bot_app_id}")
            or tab_app_id_uri.lower().endswith(f"/botid-{bot_app_id}")
        ):
            raise PackageError(
                "--tab-app-id-uri must be api://<host>/<bot app id> or api://<host>/botid-<bot app id>"
            )
        manifest["webApplicationInfo"] = {"id": bot_app_id, "resource": tab_app_id_uri}
    if tab_content_url:
        if not tab_app_id_uri:
            raise PackageError("--tab-content-url needs --tab-app-id-uri (the tab signs in with SSO)")
        content = _https_url(tab_content_url, "--tab-content-url")
        if urlsplit(content).hostname != host:
            raise PackageError("--tab-content-url must be on the --base-url host")
        manifest["staticTabs"] = [
            {
                "entityId": "neev",
                "name": "Neev",
                "contentUrl": content,
                "websiteUrl": base,
                "scopes": ["personal"],
            }
        ]
    return manifest


def validate(manifest: dict) -> list[str]:
    """Schema errors as readable strings; missing validation is a hard failure."""
    try:
        import jsonschema  # type: ignore
    except ImportError as error:
        raise PackageError(
            "Manifest validation requires jsonschema; run "
            "python3 -m pip install -r scripts/teams/requirements.txt (includes jsonschema)"
        ) from error
    try:
        schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise PackageError(f"Cannot load Teams schema {SCHEMA_PATH}; restore the vendored schema") from error
    validator = jsonschema.Draft4Validator(schema)
    return sorted(
        f"{'/'.join(str(p) for p in error.absolute_path) or '<root>'}: {error.message}"
        for error in validator.iter_errors(manifest)
    )


def _png(width: int, height: int, pixel) -> bytes:
    """A minimal RGBA PNG; `pixel(x, y)` returns an (r, g, b, a) tuple."""
    raw = bytearray()
    for y in range(height):
        raw.append(0)  # filter: none
        for x in range(width):
            raw.extend(pixel(x, y))

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def placeholder_icons() -> tuple[bytes, bytes]:
    """192x192 accent-colour tile and a 32x32 white-on-transparent outline."""
    accent = tuple(int(ACCENT[i : i + 2], 16) for i in (1, 3, 5)) + (255,)

    def color(x: int, y: int):
        inner = 48 <= x < 144 and 48 <= y < 144 and not (72 <= x < 120 and 72 <= y < 144)
        return (255, 255, 255, 255) if inner else accent

    def outline(x: int, y: int):
        edge = (6 <= x < 26 and 6 <= y < 26) and not (10 <= x < 22 and 10 <= y < 26)
        return (255, 255, 255, 255) if edge else (0, 0, 0, 0)

    return _png(192, 192, color), _png(32, 32, outline)


def _validate_icon(name: str, data: bytes, size: int) -> None:
    try:
        from PIL import Image, UnidentifiedImageError
    except ImportError as error:
        raise PackageError(
            "Icon validation requires Pillow; run "
            "python3 -m pip install -r scripts/teams/requirements.txt"
        ) from error
    try:
        with Image.open(io.BytesIO(data)) as icon:
            if icon.format != "PNG" or icon.size != (size, size):
                raise PackageError(f"--{name}-icon must be a {size}x{size} PNG file")
            icon.verify()
        with Image.open(io.BytesIO(data)) as icon:
            pixels = list(icon.convert("RGBA").getdata())
    except (OSError, ValueError, SyntaxError, UnidentifiedImageError) as error:
        raise PackageError(f"--{name}-icon must be a valid {size}x{size} PNG file") from error
    if name == "outline":
        if (
            any(alpha and (r, g, b) != (255, 255, 255) for r, g, b, alpha in pixels)
            or not any(alpha == 0 for _, _, _, alpha in pixels)
            or not any(alpha > 0 for _, _, _, alpha in pixels)
        ):
            raise PackageError("--outline-icon must contain a white mark and a transparent background")


def build_package(manifest: dict, color_png: bytes, outline_png: bytes) -> bytes:
    errors = validate(manifest)
    if errors:
        raise PackageError("manifest fails the v1.29 schema:\n  " + "\n  ".join(errors))
    _validate_icon("color", color_png, 192)
    _validate_icon("outline", outline_png, 32)
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in (
            ("manifest.json", (json.dumps(manifest, indent=2) + "\n").encode("utf-8")),
            ("color.png", color_png),
            ("outline.png", outline_png),
        ):
            info = zipfile.ZipInfo(name, ZIP_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            archive.writestr(info, data)
    return buffer.getvalue()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    env = os.environ.get
    parser.add_argument("--bot-app-id", default=env("NEEV_TEAMS_BOT_APP_ID"))
    parser.add_argument("--app-id", help="Teams app id; defaults to the bot app id")
    parser.add_argument("--base-url", required=True, help="Public Neev origin, https://…")
    parser.add_argument("--version", default="1.0.0")
    parser.add_argument("--developer-name", default="Neev")
    parser.add_argument("--privacy-url")
    parser.add_argument("--terms-url")
    parser.add_argument("--tab-app-id-uri", default=env("NEEV_TEAMS_TAB_APP_ID_URI"))
    parser.add_argument("--tab-content-url")
    parser.add_argument("--color-icon", type=Path)
    parser.add_argument("--outline-icon", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if not args.bot_app_id:
            raise PackageError("--bot-app-id (or NEEV_TEAMS_BOT_APP_ID) is required")
        manifest = build_manifest(
            bot_app_id=args.bot_app_id,
            base_url=args.base_url,
            app_id=args.app_id,
            version=args.version,
            developer_name=args.developer_name,
            privacy_url=args.privacy_url,
            terms_url=args.terms_url,
            tab_app_id_uri=args.tab_app_id_uri,
            tab_content_url=args.tab_content_url,
        )
        color, outline = placeholder_icons()
        if args.color_icon:
            color = args.color_icon.read_bytes()
        if args.outline_icon:
            outline = args.outline_icon.read_bytes()
        package = build_package(manifest, color, outline)
    except (PackageError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(package)
    print(f"wrote {args.output} (Teams app id {manifest['id']}, bot {manifest['bots'][0]['botId']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
