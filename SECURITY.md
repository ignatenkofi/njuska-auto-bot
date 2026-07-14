# Security Policy

## Reporting a vulnerability

Please report vulnerabilities **privately** through GitHub's built-in form:

**[Report a vulnerability](https://github.com/ignatenkofi/njuska-auto-bot/security/advisories/new)**
(repository → *Security* tab → *Report a vulnerability*).

Do **not** open a public issue for security problems — public issues are
visible immediately, before a fix exists.

If the form is unavailable for any reason, contact the maintainer through a
private channel listed on their GitHub profile instead.

### What to include

- What the bug lets an attacker do (impact), and who can trigger it.
- Steps to reproduce — a minimal `.env`/setup and the exact input, if relevant.
- Affected version (`njuska_auto_bot --version` or the release tag).

## Supported versions

This is a small hobby project. Only the **latest release** (and `main`)
receives fixes. There are no backports — if you run an older tag, the fix is
"update".

| Version        | Supported |
| -------------- | --------- |
| latest release | yes       |
| older releases | no        |

## What to expect

Response and fixes are **best effort** — this project is maintained by one
person in their spare time. Realistically:

- An acknowledgement within about a week.
- A fix or a decision (including "won't fix, here's why") as time permits;
  genuine token/secret-exposure issues get priority.
- Credit in the release notes if you want it.

## Scope notes

Things that are *known and accepted* trade-offs, not vulnerabilities:

- The bot trusts every configured operator in `AUTHORIZED_USER_ID` (a single
  id or a comma-separated list) equally; there are no per-user roles or
  permission tiers — anyone on the list can run every command.
- HTML fetched from polovniautomobili.com is treated as untrusted input;
  escaping bugs in what the bot forwards to Telegram **are** in scope.
- Secrets live in `.env` / systemd environment on the deployment box —
  local machine compromise is out of scope.
