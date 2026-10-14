"""Teams app package generator: manifest shape against the vendored v1.29
schema, refused inputs, icons and the zip layout Teams admin center accepts."""

import io
from contextlib import redirect_stderr
import json
import struct
import sys
import tempfile
import unittest
import unittest.mock
import zipfile
import zlib
from pathlib import Path

TEAMS = Path(__file__).parents[1] / "teams"
sys.path.insert(0, str(TEAMS))

import build_app_package as pkg  # noqa: E402

BOT = "11111111-2222-3333-4444-555555555555"
BASE = "https://neev.example.com"


def png_size(data: bytes) -> tuple[int, int, int]:
    """(width, height, colour type) from the IHDR chunk."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    width, height, _depth, colour = struct.unpack(">IIBB", data[16:26])
    return width, height, colour


def png_pixels(data: bytes) -> list[tuple[int, int, int, int]]:
    width, height, _ = png_size(data)
    idat = data[data.index(b"IDAT") + 4 : data.index(b"IEND") - 8]
    raw = zlib.decompress(idat)
    stride = 1 + width * 4
    return [
        tuple(raw[y * stride + 1 + x * 4 : y * stride + 5 + x * 4])
        for y in range(height)
        for x in range(width)
    ]


class ManifestTests(unittest.TestCase):
    def test_invalid_authorities_fail_without_writing_package(self):
        for host in ("example.com\\evil", "%23.example.com", "bad_host.example",
                     "-bad.example", "example..com", "999.1.1.1", "example.com:",
                     "[::1]junk", "[fe80::1%25en0]"):
            with self.subTest(host=host), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "app.zip"
                stderr = io.StringIO()
                with redirect_stderr(stderr):
                    result = pkg.main(["--bot-app-id", BOT, "--base-url", "https://" + host,
                                       "--output", str(output)])
                self.assertEqual(result, 2)
                self.assertFalse(output.exists())
                self.assertIn("--base-url", stderr.getvalue())

    def test_valid_dns_idna_and_ip_authorities(self):
        for authority in ("example.com", "EXAMPLE.com:443", "xn--bcher-kva.example",
                          "bücher.example", "127.0.0.1:8443", "[2001:db8::1]:443"):
            with self.subTest(authority=authority):
                manifest = pkg.build_manifest(bot_app_id=BOT, base_url="https://" + authority)
                self.assertEqual(pkg.validate(manifest), [])

    def test_minimal_manifest_is_schema_valid(self):
        manifest = pkg.build_manifest(bot_app_id=BOT.upper(), base_url=BASE + "/")
        self.assertEqual(pkg.validate(manifest), [])
        self.assertEqual(manifest["manifestVersion"], "1.29")
        self.assertEqual(manifest["id"], BOT)
        self.assertEqual(manifest["bots"][0]["botId"], BOT)
        self.assertEqual(manifest["bots"][0]["scopes"], ["personal", "team"])
        self.assertEqual(manifest["validDomains"], ["neev.example.com"])
        self.assertEqual(manifest["developer"]["privacyUrl"], BASE + "/privacy")
        self.assertNotIn("webApplicationInfo", manifest)
        self.assertNotIn("staticTabs", manifest)

    def test_tab_sso_and_static_tab_are_opt_in(self):
        uri = f"api://neev.example.com/botid-{BOT}"
        manifest = pkg.build_manifest(
            bot_app_id=BOT,
            base_url=BASE,
            app_id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            tab_app_id_uri=uri,
            tab_content_url=BASE + "/teams/tab",
        )
        self.assertEqual(pkg.validate(manifest), [])
        self.assertEqual(manifest["id"], "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        self.assertEqual(manifest["webApplicationInfo"], {"id": BOT, "resource": uri})
        self.assertEqual(manifest["staticTabs"][0]["contentUrl"], BASE + "/teams/tab")

    def test_refused_inputs(self):
        cases = [
            dict(bot_app_id="not-a-guid", base_url=BASE),
            dict(bot_app_id=BOT, base_url="http://neev.example.com"),
            dict(bot_app_id=BOT, base_url="https://u:p@neev.example.com"),
            dict(bot_app_id=BOT, base_url=BASE, version="1.0"),
            dict(bot_app_id=BOT, base_url=BASE, developer_name="x" * 33),
            dict(bot_app_id=BOT, base_url=BASE, tab_app_id_uri="api://neev.example.com/other"),
            dict(bot_app_id=BOT, base_url=BASE, tab_content_url=BASE + "/tab"),
            dict(
                bot_app_id=BOT,
                base_url=BASE,
                tab_app_id_uri=f"api://neev.example.com/{BOT}",
                tab_content_url="https://elsewhere.example.com/tab",
            ),
        ]
        for case in cases:
            with self.subTest(case=case), self.assertRaises(pkg.PackageError):
                pkg.build_manifest(**case)

    def test_base_must_be_clean_origin_but_explicit_page_paths_work(self):
        for suffix in ("/#fragment", "#", "/prefix", "?", "?key=value"):
            with self.subTest(suffix=suffix), self.assertRaises(pkg.PackageError):
                pkg.build_manifest(bot_app_id=BOT, base_url=BASE + suffix)
        manifest = pkg.build_manifest(
            bot_app_id=BOT, base_url=BASE,
            privacy_url=BASE + "/legal/privacy", terms_url=BASE + "/legal/terms",
        )
        self.assertEqual(manifest["developer"]["privacyUrl"], BASE + "/legal/privacy")
        self.assertEqual(manifest["developer"]["termsOfUseUrl"], BASE + "/legal/terms")

    def test_schema_catches_a_broken_manifest(self):
        manifest = pkg.build_manifest(bot_app_id=BOT, base_url=BASE)
        manifest["name"]["short"] = "x" * 31
        manifest["bots"][0]["scopes"] = ["everywhere"]
        errors = pkg.validate(manifest)
        self.assertTrue(any(error.startswith("name/short") for error in errors), errors)
        self.assertTrue(any(error.startswith("bots/0/scopes") for error in errors), errors)


class PackageTests(unittest.TestCase):
    def test_placeholder_icons_meet_teams_rules(self):
        color, outline = pkg.placeholder_icons()
        self.assertEqual(png_size(color), (192, 192, 6))
        self.assertEqual(png_size(outline), (32, 32, 6))
        # Outline: only white or fully transparent pixels.
        pixels = set(png_pixels(outline))
        self.assertTrue(pixels <= {(255, 255, 255, 255), (0, 0, 0, 0)}, pixels)
        self.assertIn((0, 0, 0, 0), pixels)
        self.assertIn((255, 255, 255, 255), pixels)

    def test_package_layout_is_flat_and_reproducible(self):
        manifest = pkg.build_manifest(bot_app_id=BOT, base_url=BASE)
        color, outline = pkg.placeholder_icons()
        first = pkg.build_package(manifest, color, outline)
        self.assertEqual(first, pkg.build_package(manifest, color, outline))
        with zipfile.ZipFile(io.BytesIO(first)) as archive:
            self.assertEqual(
                sorted(archive.namelist()), ["color.png", "manifest.json", "outline.png"]
            )
            self.assertEqual(json.loads(archive.read("manifest.json")), manifest)

    def test_invalid_manifest_cannot_be_packaged_directly(self):
        manifest = pkg.build_manifest(bot_app_id=BOT, base_url=BASE)
        manifest["bots"][0]["scopes"] = ["everywhere"]
        with self.assertRaisesRegex(pkg.PackageError, "schema"):
            pkg.build_package(manifest, *pkg.placeholder_icons())

    def test_cli_requires_schema_dependency_without_writing_output(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "package.zip"
            stderr = io.StringIO()
            with unittest.mock.patch.dict(sys.modules, {"jsonschema": None}), redirect_stderr(stderr):
                code = pkg.main(["--bot-app-id", BOT, "--base-url", BASE, "--output", str(output)])
            self.assertEqual(code, 2)
            self.assertFalse(output.exists())
            self.assertIn("pip install", stderr.getvalue())
            self.assertIn("jsonschema", stderr.getvalue())

    def test_cli_fails_actionably_when_vendored_schema_is_missing(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "package.zip"
            stderr = io.StringIO()
            with unittest.mock.patch.object(pkg, "SCHEMA_PATH", Path(directory) / "missing.json"), redirect_stderr(stderr):
                code = pkg.main(["--bot-app-id", BOT, "--base-url", BASE, "--output", str(output)])
            self.assertEqual(code, 2)
            self.assertFalse(output.exists())
            self.assertIn("restore the vendored schema", stderr.getvalue())

    def test_missing_image_decoder_refuses_package(self):
        manifest = pkg.build_manifest(bot_app_id=BOT, base_url=BASE)
        with unittest.mock.patch.dict(sys.modules, {"PIL": None}):
            with self.assertRaisesRegex(pkg.PackageError, "requires Pillow.*pip install"):
                pkg.build_package(manifest, *pkg.placeholder_icons())

    def test_white_antialiased_outline_with_transparent_background_is_accepted(self):
        manifest = pkg.build_manifest(bot_app_id=BOT, base_url=BASE)
        outline = pkg._png(32, 32, lambda x, y: (255, 255, 255, 128 if x < 16 else 0))
        self.assertTrue(zipfile.is_zipfile(io.BytesIO(
            pkg.build_package(manifest, pkg.placeholder_icons()[0], outline)
        )))

    def test_supplied_icons_require_dimensions_and_white_transparent_outline(self):
        manifest = pkg.build_manifest(bot_app_id=BOT, base_url=BASE)
        color, outline = pkg.placeholder_icons()
        cases = [
            (pkg._png(32, 32, lambda x, y: (255, 255, 255, 255)), outline),
            (color, pkg._png(32, 32, lambda x, y: (255, 0, 0, 255))),
            (color, pkg._png(32, 32, lambda x, y: (255, 255, 255, 255))),
            (color, b"\x89PNG\r\n\x1a\n"),
        ]
        for icons in cases:
            with self.subTest(), self.assertRaises(pkg.PackageError):
                pkg.build_package(manifest, *icons)

    def test_non_png_icon_is_refused(self):
        manifest = pkg.build_manifest(bot_app_id=BOT, base_url=BASE)
        with self.assertRaises(pkg.PackageError):
            pkg.build_package(manifest, b"GIF89a", pkg.placeholder_icons()[1])

    def test_cli_writes_package_and_refuses_missing_bot(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "out" / "neev-teams.zip"
            code = pkg.main(["--bot-app-id", BOT, "--base-url", BASE, "--output", str(output)])
            self.assertEqual(code, 0)
            self.assertTrue(zipfile.is_zipfile(output))
            missing = Path(directory) / "missing.zip"
            with unittest.mock.patch.dict("os.environ", {}, clear=True):
                code = pkg.main(["--base-url", BASE, "--output", str(missing)])
            self.assertEqual(code, 2)
            self.assertFalse(missing.exists())


if __name__ == "__main__":
    unittest.main()
