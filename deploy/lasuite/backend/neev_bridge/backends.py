"""Authentication backend resolving Docs users by Neev subject.

`core.models.User` sets `USERNAME_FIELD = "admin_email"`, so Django's stock
`RemoteUserBackend` — which looks the user up by username field — would resolve a
Neev subject against an *email* column. This backend resolves on `sub`, which is
the field upstream itself treats as the external identity key.

Email and display name are written from the advisory headers, and are never used
to find or match a user.
"""

import logging

from django.contrib.auth import get_user_model
from django.core.exceptions import ValidationError
from django.contrib.auth.backends import RemoteUserBackend
from django.db import IntegrityError, transaction

from neev_bridge.identity import clean_profile_value, parse_subject

logger = logging.getLogger(__name__)

EMAIL_HEADER = "HTTP_X_NEEV_REMOTE_EMAIL"
NAME_HEADER = "HTTP_X_NEEV_REMOTE_NAME"

# core.models.User field limits.
EMAIL_MAX_LENGTH = 254
FULL_NAME_MAX_LENGTH = 100
SHORT_NAME_MAX_LENGTH = 100


def profile_from_request(request):
    """Advisory profile fields carried alongside the identity header."""
    if request is None:
        return {}
    meta = getattr(request, "META", {})
    profile = {}
    email = clean_profile_value(meta.get(EMAIL_HEADER), EMAIL_MAX_LENGTH)
    if email and "@" in email:
        profile["email"] = email
    full_name = clean_profile_value(meta.get(NAME_HEADER), FULL_NAME_MAX_LENGTH)
    if full_name:
        profile["full_name"] = full_name
    return profile


def display_defaults(subject, profile):
    """Names to use only when the stored ones are empty.

    Upstream assumes an email always exists:
    `core.api.serializers.UserSerializer.get_full_name` (and `get_short_name`)
    fall back to `instance.email.split("@")[0]` when the name is empty. Under
    OIDC that holds; under header identity the email is optional, so those
    serializers raise `AttributeError` on None — a 500 on `/users/me/` and on
    anything that embeds a user. Found by running against the real image.

    Rather than make the email header mandatory — Neev cannot promise every
    account has a verified email — the bridge guarantees the fields upstream
    actually reads. These are *defaults*, applied only to an empty field, so a
    request that happens to omit the name header never overwrites a real name
    with a placeholder.
    """
    fallback = profile.get("full_name")
    if not fallback:
        email = profile.get("email")
        fallback = email.split("@")[0] if email else f"Neev {subject.user_id}"
    return {
        "full_name": fallback[:FULL_NAME_MAX_LENGTH],
        "short_name": fallback[:SHORT_NAME_MAX_LENGTH],
    }


def get_or_create_user_by_subject(subject, profile=None):
    """Resolve — or create — the Docs user for a validated subject.

    Idempotent, and safe under concurrent first requests for the same subject:
    `sub` carries a unique constraint, and the loser of a race re-reads the row
    the winner committed instead of failing.

    Not `get_or_create`: `core.models.BaseModel.save()` calls `full_clean()`,
    and a user built with `defaults` alone has a blank password, which
    `full_clean` rejects. The password has to be set before the first save, not
    patched in afterwards. These accounts never authenticate with one — the
    unusable hash says so explicitly rather than leaving an empty field behind.
    """
    user_model = get_user_model()
    profile = dict(profile or {})
    defaults = display_defaults(subject, profile)

    user = user_model.objects.filter(sub=subject.raw).first()
    if user is not None:
        refresh_profile(user, profile, defaults)
        clear_first_connection(user)
        return user, False

    user = user_model(sub=subject.raw, **{**defaults, **profile})
    # Onboarding is Neev's, not Docs'. See `clear_first_connection`.
    if hasattr(user, "is_first_connection"):
        user.is_first_connection = False
    user.set_unusable_password()
    try:
        with transaction.atomic():
            user.save()
    except (IntegrityError, ValidationError):
        # Both, because a race can surface either one. `sub` is unique, so a
        # concurrent creation that has already *committed* is found by
        # `full_clean`'s uniqueness check and raised as a ValidationError, while
        # one still in flight collides at the constraint and raises
        # IntegrityError. Catching only the second left the first as a 500 under
        # exactly the load the retry exists for.
        user = user_model.objects.filter(sub=subject.raw).first()
        if user is None:
            # Not a race: the record is genuinely invalid.
            raise
        refresh_profile(user, profile, defaults)
        return user, False
    return user, True


def clear_first_connection(user):
    """Turn off Docs' onboarding for a bridge-managed account.

    `core.models.User.is_first_connection` defaults to True, and the frontend
    mounts `<FirstConnection/>` on it
    (`features/auth/components/Auth.tsx:117`), which opens Docs' own onboarding
    modal over the editor. Closing it calls `users/onboarding-done/`
    (`features/auth/api/useOnboardingDone.tsx:17`), a route the proxy's
    allowlist does not contain — so in the embed the modal cannot be dismissed
    for good and comes back on every load. Its slides also reference
    `/assets/on-boarding` without the engine prefix, so they would 404 anyway.

    Onboarding belongs to Neev, which is why this is switched off rather than
    proxied: the alternative is exposing an onboarding API and rebuilding the
    frontend to fix asset paths, for a flow the product does not use.

    Applied on creation and, for accounts that predate this, on the next
    request — normalising existing rows without a migration. Only users the
    bridge resolves are touched, and it only ever resolves validated `neev:`
    subjects, so a non-Neev account (an operator's, say) keeps Docs' behaviour.
    """
    if getattr(user, "is_first_connection", False) is not True:
        return False
    user.is_first_connection = False
    with transaction.atomic():
        user.save(update_fields=["is_first_connection"])
    return True


def refresh_profile(user, profile, defaults=None):
    """Write changed advisory fields back, without touching identity.

    `profile` is what Neev asserted this time and wins over what is stored.
    `defaults` only fill a field that is currently empty.
    """
    updates = {
        field: value
        for field, value in profile.items()
        if value and getattr(user, field, None) != value
    }
    for field, value in (defaults or {}).items():
        if not getattr(user, field, None) and field not in updates:
            updates[field] = value

    if not updates:
        return False
    for field, value in updates.items():
        setattr(user, field, value)
    with transaction.atomic():
        user.save(update_fields=list(updates))
    return True


class NeevRemoteUserBackend(RemoteUserBackend):
    """RemoteUserBackend keyed on `User.sub` instead of the username field."""

    create_unknown_user = True

    def authenticate(self, request, remote_user=None, **kwargs):
        """Resolve the asserted subject to a user, or refuse."""
        subject = parse_subject(remote_user)
        if subject is None:
            # Fail closed. An unparsable assertion is not a weak identity, it is
            # no identity, and must not create or resolve an account.
            if remote_user:
                logger.warning("neev-bridge: refused malformed remote subject")
            return None

        user, created = get_or_create_user_by_subject(
            subject, profile_from_request(request)
        )
        if created:
            logger.info("neev-bridge: provisioned user for a new subject")

        if not self.user_can_authenticate(user):
            return None
        return user

    def clean_username(self, username):
        """Subjects are used verbatim; no normalisation that could collide."""
        return username
