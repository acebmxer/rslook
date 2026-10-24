# Rslook

[![CI](https://github.com/acebmxer/rslook/actions/workflows/ci.yml/badge.svg?branch=dev)](https://github.com/acebmxer/rslook/actions/workflows/ci.yml)

A clean-room, open-source reimplementation of **Outlook classic** (the Windows desktop
mail/calendar/contacts client) in pure Rust, for Linux and Windows. Modelled on the ArtCraft
Crafting Apps (WordCraft, GridCraft, …): egui on the GPU, every action a command, never crash.

Status: pre-alpha. The three-pane mail window works against a local sample mailbox, a real
IMAP/SMTP account or a Microsoft 365 / Outlook.com account, with an offline cache.

## Run

```
cargo run --release -p rslook            # sample mailbox until you add an account (File tab)
cargo run --release -p rslook -- a.eml   # also import .eml, .msg or .pst files
cargo test
scripts/mail-test.sh                     # end-to-end Send/Receive against throwaway mail servers (podman)
cargo run -p rslook-ui-egui --example ui_shot -- /tmp/shot.png mail.reply
cargo run -p rslook-pst --features fixtures --example make_sample -- /tmp   # synthetic .pst and .msg to import
```

## What works

- Ribbon (File, Home, Send / Receive, Folder, View, Help), folder pane with Favorites and unread
  counts, message list grouped by date, reading pane (right / bottom / off), navigation bar.
- New, Reply, Reply All, Forward, Send (to Sent Items), Save draft, Delete (to Deleted Items,
  then permanent), Archive, Junk, Move, Mark read/unread, Follow up, Search, Undo/Redo.
- Outlook classic keyboard shortcuts (Ctrl+N/R/Shift+R/F/Q/U/E/Enter, Delete, Backspace, Insert,
  Ctrl+1..4, Ctrl+. / Ctrl+,, F9, Shift+F9).
- Import/Export (File tab): an Outlook data file (`.pst`, Unicode or ANSI, read-only through
  Microsoft's open-source [MS-PST] reader) is copied into a new folder named after the file,
  with its folder tree inside; a single message (`.msg` per [MS-OXMSG], or RFC 5322 / MIME
  `.eml`) goes into the current folder. Calendar items, contacts, tasks and notes are skipped
  for now, and attachments are listed by name and size. Imports are kept in the account's
  offline cache; without an account they go into the sample mailbox and are gone after closing.
- Accounts: one IMAP (RFC 9051) + SMTP account, added on the File tab (Add Account / Account
  Settings). SSL/TLS, STARTTLS or (for test servers) no encryption; certificates are checked
  against the system trust store. The password goes to the OS keychain (Secret Service on Linux,
  Credential Manager on Windows), never to a file.
- Microsoft 365 / Outlook.com accounts (Add Account, account type Microsoft 365 / Outlook.com):
  you sign in on Microsoft's own page in your web browser (OAuth 2.0 with PKCE); Rslook never
  sees the password and keeps only the sign-in token, in the OS keychain. Mail goes through
  Microsoft Graph, or through IMAP and SMTP with OAuth (XOAUTH2) if you pick that instead.
  Sign-in needs an application (client) ID registered in Microsoft Entra; until Rslook ships its
  own, enter yours in the account dialog. Work or school tenants may need their admin to allow
  the app.
- Send / Receive tab: Send/Receive All Folders (F9), Update Folder (Shift+F9), Send All, Work
  Offline. Sent mail waits in the Outbox, goes out over SMTP and a copy is saved to Sent Items.
  Folders, flags (read, flagged), moves and deletes sync both ways.
- Offline cache: synced folders and messages stay readable with no connection; changes made
  offline are queued, survive a restart and replay on the next Send/Receive.

## Roadmap

1. Calendar (iCalendar, CalDAV), People (vCard, CardDAV), Tasks.
2. HTML mail rendering, rich compose, rules, Quick Steps, search folders.

Not planned: Exchange Web Services (Microsoft switches it off in Exchange Online from October
2026; on-premises Exchange can be used over IMAP) and Gmail sign-in (Google allows it only for
apps that pass its paid security review; Gmail still works over IMAP with an app password).

## Clean room

See `AGENTS.md`. Outlook is only observed black-box; its binaries are never decompiled or read,
and no Microsoft assets or wording are used.

## License

MIT OR Apache-2.0.
