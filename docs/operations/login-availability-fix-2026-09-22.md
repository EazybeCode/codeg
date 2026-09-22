# Login availability fix — 2026-09-22

## Problem and change
Anonymous session requests correctly return 401. The parallel auth bootstrap used Promise.all, so that response discarded successful public capabilities and disabled every sign-in method. Commit ba9b3588 handles the two results independently with Promise.allSettled while retaining abort/generation guards and failing closed for invalid capabilities. No private session is restored on 401/403.

## Validation
- Regression test failed on the disabled Google button before the fix.
- 102 affected tests passed; lint, TypeScript and production build passed.
- Independent Claude review found no material issues.
- Anonymous browser reproduced the old disabled screen. Candidate browser using live public configuration enabled Google and email; entering a dummy address enabled the email request button without submitting it.
- Google initiation returned the expected Google OAuth authorization endpoint. Full identity sign-in and email delivery were not exercised.

## Deployment
- Image: neev-native-agent:ux-login-ba9b3588
- Workspace HTML SHA256: cf30b02cfd2c0919d6ee84c78e9495cc1cca470754aece9525a93ddfa348eb90
- Live artifact hash verified; schema remains 13.
- Post-deploy anonymous browser check against production confirmed Google and email request controls enabled (valid dummy email entered, no submission).
- Database backup: neev-before-login-fix-20260922-040129.sql
- Compose backup: compose.yml.before-login-fix-20260922-040129
- Rollback image: neev-native-agent:ux-unified-ada28f1b
- GitHub remains unavailable because that provider is not configured.
