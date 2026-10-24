# Rslook

A clean-room, open-source reimplementation of **Outlook classic** (the Windows desktop
mail/calendar/contacts client) in pure Rust, for Linux and Windows. Modelled on the ArtCraft
Crafting Apps (WordCraft, GridCraft, …): egui on the GPU, every action a command, never crash.

Status: pre-alpha. The three-pane mail window works against a local sample mailbox or a real
IMAP/SMTP account, with an offline cache.

## Run

```
cargo run --release -p rslook            # sample mailbox until you add an account (File tab)
cargo run --release -p rslook -- a.eml   # also import .eml files
cargo test
scripts/mail-test.sh                     # end-to-end Send/Receive against throwaway mail servers (podman)
cargo run -p rslook-ui-egui --example ui_shot -- /tmp/shot.png mail.reply
```

## What works

- Ribbon (File, Home, Send / Receive, Folder, View, Help), folder pane with Favorites and unread
  counts, message list grouped by date, reading pane (right / bottom / off), navigation bar.
- New, Reply, Reply All, Forward, Send (to Sent Items), Save draft, Delete (to Deleted Items,
  then permanent), Archive, Junk, Move, Mark read/unread, Follow up, Search, Undo/Redo.
- Outlook classic keyboard shortcuts (Ctrl+N/R/Shift+R/F/Q/U/E/Enter, Delete, Backspace, Insert,
  Ctrl+1..4, Ctrl+. / Ctrl+,, F9, Shift+F9).
- RFC 5322 / MIME import (`.eml`).
- Accounts: one IMAP (RFC 9051) + SMTP account, added on the File tab (Add Account / Account
  Settings). SSL/TLS, STARTTLS or (for test servers) no encryption; certificates are checked
  against the system trust store. The password goes to the OS keychain (Secret Service on Linux,
  Credential Manager on Windows), never to a file.
- Send / Receive tab: Send/Receive All Folders (F9), Update Folder (Shift+F9), Send All, Work
  Offline. Sent mail waits in the Outbox, goes out over SMTP and a copy is saved to Sent Items.
  Folders, flags (read, flagged), moves and deletes sync both ways.
- Offline cache: synced folders and messages stay readable with no connection; changes made
  offline are queued, survive a restart and replay on the next Send/Receive.

## Roadmap

1. `.pst` / `.msg` import from the Microsoft Open Specifications ([MS-PST], [MS-OXMSG]).
2. Microsoft 365 / Exchange (Graph, EWS) and Gmail with OAuth.
3. Calendar (iCalendar, CalDAV), People (vCard, CardDAV), Tasks.
4. HTML mail rendering, rich compose, rules, Quick Steps, search folders.

## Clean room

See `AGENTS.md`. Outlook is only observed black-box; its binaries are never decompiled or read,
and no Microsoft assets or wording are used.

## License

MIT OR Apache-2.0.
