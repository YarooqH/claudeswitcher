# Security Policy

## Supported versions

Only the latest commit on `main` is supported. There are no tagged releases yet.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem.

Report it privately through GitHub: open the repository's **Security** tab and choose
**Report a vulnerability**. Include what you found, how to reproduce it, and what an attacker
could do with it.

You can expect an acknowledgement within a few days. Fixes are made on a private branch where
possible and published together with a short advisory.

## What is in scope

ClaudeSyncTray runs locally and touches sensitive files (Claude Desktop sign-in data and the
Claude Code CLI credentials), so these matter most:

- Leaking, copying or overwriting credentials or session data outside the places the app
  documents.
- Deleting or overwriting files the app did not create, for example by following a junction or
  symlink.
- Anything that lets another local process or user read or change an account's data through
  the app.

Problems in Claude Desktop, Claude Code or Windows themselves should be reported to their
vendors.
