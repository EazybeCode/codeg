"""Subject parsing: what is accepted, and what is refused."""

from django.test import SimpleTestCase

from neev_bridge.identity import (
    MAX_SUBJECT_LENGTH,
    SERVICE_SUBJECT,
    is_reserved_subject,
    parse_browser_subject,
    parse_subject,
)


class ParseSubjectTests(SimpleTestCase):
    """The grammar of `X-Neev-Remote-User`."""

    def test_accepts_integer_and_uuid_style_ids(self):
        for raw in (
            "neev:1:23",
            "neev:42:7",
            "neev:9f1c2f8e-1d3a-4f6b-8c2e-7a1b2c3d4e5f:2b6b8c1d-0e4f-4a2b-9c3d-5e6f7a8b9c0d",
            "neev:org_1:user-2",
        ):
            with self.subTest(raw=raw):
                self.assertIsNotNone(parse_subject(raw))

    def test_segments_are_recovered_unambiguously(self):
        """`neev:1:23` and `neev:12:3` are different principals, always."""
        first = parse_subject("neev:1:23")
        second = parse_subject("neev:12:3")
        self.assertEqual((first.org_id, first.user_id), ("1", "23"))
        self.assertEqual((second.org_id, second.user_id), ("12", "3"))
        self.assertNotEqual(first, second)

    def test_refuses_malformed_values(self):
        for raw in (
            None,
            "",
            "neev:",
            "neev::1",
            "neev:1:",
            "neev:1",
            "neev:1:2:3",  # an extra segment must not be silently truncated
            "other:1:2",
            "neev:1:2 ",  # trailing whitespace is malformed, not repairable
            " neev:1:2",
            "neev:1:2\n",
            "neev:1:2\nX-Other: injected",
            "neev:1:2/../3",
            "neev:1:é",  # non-ASCII: the model validator refuses it too
            "neev:" + "a" * 200 + ":" + "b" * 200,
            b"neev:1:2",
            12,
        ):
            with self.subTest(raw=raw):
                self.assertIsNone(parse_subject(raw))

    def test_length_is_bounded_by_the_column(self):
        raw = "neev:1:" + "a" * MAX_SUBJECT_LENGTH
        self.assertIsNone(parse_subject(raw))


class ReservedNamespaceTests(SimpleTestCase):
    """The service principal is not a browser identity."""

    def test_service_subject_parses_on_the_internal_path(self):
        subject = parse_subject(SERVICE_SUBJECT)
        self.assertIsNotNone(subject)
        self.assertTrue(subject.is_service)

    def test_service_subject_is_refused_on_the_browser_path(self):
        self.assertIsNone(parse_browser_subject(SERVICE_SUBJECT))
        self.assertIsNone(parse_browser_subject("neev:service:anything"))

    def test_ordinary_subject_is_not_reserved(self):
        self.assertFalse(is_reserved_subject("neev:1:2"))
        self.assertIsNotNone(parse_browser_subject("neev:1:2"))
