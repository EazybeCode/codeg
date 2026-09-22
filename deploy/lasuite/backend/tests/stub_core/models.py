"""A User model mirroring `core.models.User` where the bridge depends on it.

This is not a convenience double. Two properties are copied deliberately,
because they are exactly what the bridge has to cope with:

* `USERNAME_FIELD = "admin_email"`, so `get_username()` returns something that
  can never equal a Neev subject — the trap `NeevRemoteUserMiddleware` exists to
  avoid;
* `sub` is a nullable, unique CharField — the real identity key;
* `save()` calls `full_clean()`, as upstream's `BaseModel` does. That one is
  here because the in-image run caught what its absence hid: a user created
  without a password passes on a plain model and is rejected by upstream's.

Everything else upstream's model carries (timezone, language, OIDC bookkeeping)
is irrelevant here and left out rather than approximated.
"""

from django.contrib.auth.models import AbstractBaseUser, PermissionsMixin
from django.db import models


class User(AbstractBaseUser, PermissionsMixin):
    """Mirror of the fields the bridge reads and writes."""

    sub = models.CharField(max_length=255, unique=True, blank=True, null=True)
    full_name = models.CharField(max_length=100, blank=True, null=True)
    short_name = models.CharField(max_length=100, blank=True, null=True)
    email = models.EmailField(blank=True, null=True)
    admin_email = models.EmailField(unique=True, blank=True, null=True)
    is_active = models.BooleanField(default=True)
    is_staff = models.BooleanField(default=False)

    USERNAME_FIELD = "admin_email"
    REQUIRED_FIELDS = []

    class Meta:
        app_label = "stub_core"

    def save(self, *args, **kwargs):
        """Validate before saving, exactly as `core.models.BaseModel` does."""
        self.full_clean()
        super().save(*args, **kwargs)
