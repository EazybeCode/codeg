"""Settings for the local (no-container) half of the bridge tests.

These exercise the real Django 5.2.17 request path — SessionMiddleware,
AuthenticationMiddleware, `auth.login()`, cookie handling — against the mirrored
User model. What they cannot cover is anything that needs `core`: provisioning,
ACL projection and the real settings module. Those are covered by
`tests/container/selftest.py`, run inside the pinned image.
"""

SECRET_KEY = "local-tests-only-not-a-deployment-secret"
DEBUG = False
ALLOWED_HOSTS = ["testserver"]

INSTALLED_APPS = [
    "django.contrib.contenttypes",
    "django.contrib.auth",
    "django.contrib.sessions",
    "tests.stub_core",
]

AUTH_USER_MODEL = "stub_core.User"

MIDDLEWARE = [
    "django.contrib.sessions.middleware.SessionMiddleware",
    "django.middleware.csrf.CsrfViewMiddleware",
    "django.contrib.auth.middleware.AuthenticationMiddleware",
    "neev_bridge.middleware.NeevRemoteUserMiddleware",
]

AUTHENTICATION_BACKENDS = [
    "neev_bridge.backends.NeevRemoteUserBackend",
    "django.contrib.auth.backends.ModelBackend",
]

# The database-backed session store keeps the test honest about session keys
# being created, cycled and flushed for real.
SESSION_ENGINE = "django.contrib.sessions.backends.db"
SESSION_COOKIE_NAME = "docs_sessionid"
# Cookie *scoping* is asserted in the container tests against the real settings
# module; here the path is left at the default so the test client's cookie jar
# does not become part of what is under test.


ROOT_URLCONF = "tests.urls_local"

DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.sqlite3",
        "NAME": ":memory:",
    }
}

USE_TZ = True
DEFAULT_AUTO_FIELD = "django.db.models.AutoField"
PASSWORD_HASHERS = ["django.contrib.auth.hashers.MD5PasswordHasher"]
