Working note for building Roadmap step 1 (Accounts). Not documentation.

# Accounts: IMAP, SMTP, OS keychain, offline cache

## Decided

- Libraries (MIT / Apache-2.0): `async-imap`, `lettre` (SMTP), `keyring` (OS keychain), `rustls`, `tokio`.
- Tests use a throwaway GreenMail container with synthetic mail only. No real accounts.
- Builds and tests run in the `rslook-dev` Podman container, never on the host.
- New work gets no GitHub issue (issue #1 was closed as not planned).

## State

- `dev` is at `c9aecbb` (rustfmt + clippy fixes), clean, and pushed as `origin/dev`.
- fmt, clippy and the 19 tests pass in the container.
- No Accounts code written yet.

## Open question

- rslook has no `CHANGELOG.md`. Asked whether to create one with an empty `## [Unreleased]`; no answer yet.

## Scope of the stage (all of it, built and tested)

1. Account model in `crates/engine`: IMAP and SMTP host, port, TLS mode, user name. The password goes in the OS keychain, never on disk.
2. IMAP (RFC 9051, IMAP4rev1 fallback): list folders, fetch headers and bodies, flags (read, flagged, deleted), move, delete, append to Sent.
3. SMTP (RFC 5321) submission with STARTTLS or implicit TLS, wired to Send.
4. Offline cache: synced folders and messages readable with no connection; offline changes queued and replayed.
5. Send / Receive ribbon commands (Send/Receive All Folders, Update Folder) as `CommandSpec`s, with catalog entries.
6. UI: account dialog and sync status in `crates/ui-egui`.
7. New crates stay below L6 (no egui) and start with the `#![deny(clippy::unwrap_used, …)]` header; hostile-params test stays green.
8. README (What works, Roadmap) and CHANGELOG updated.

Delete this file once Accounts lands.
