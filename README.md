# Rslook

A clean-room, open-source reimplementation of **Outlook classic** (the Windows desktop
mail/calendar/contacts client) in pure Rust, for Linux and Windows. Modelled on the ArtCraft
Crafting Apps (WordCraft, GridCraft, …): egui on the GPU, every action a command, never crash.

Status: pre-alpha. The three-pane mail window works against a local sample mailbox; accounts
and sync are next.

## Run

```
cargo run --release -p rslook            # sample mailbox
cargo run --release -p rslook -- a.eml   # also import .eml files
cargo test
cargo run -p rslook-ui-egui --example ui_shot -- /tmp/shot.png mail.reply
```

## What works

- Ribbon (File, Home, Send / Receive, Folder, View, Help), folder pane with Favorites and unread
  counts, message list grouped by date, reading pane (right / bottom / off), navigation bar.
- New, Reply, Reply All, Forward, Send (to Sent Items), Save draft, Delete (to Deleted Items,
  then permanent), Archive, Junk, Move, Mark read/unread, Follow up, Search, Undo/Redo.
- Outlook classic keyboard shortcuts (Ctrl+N/R/Shift+R/F/Q/U/E/Enter, Delete, Backspace, Insert,
  Ctrl+1..4, Ctrl+. / Ctrl+,).
- RFC 5322 / MIME import (`.eml`).

## Roadmap

1. Accounts: IMAP (RFC 9051) + SMTP, OS keychain, offline cache.
2. `.pst` / `.msg` import from the Microsoft Open Specifications ([MS-PST], [MS-OXMSG]).
3. Microsoft 365 / Exchange (Graph, EWS) and Gmail with OAuth.
4. Calendar (iCalendar, CalDAV), People (vCard, CardDAV), Tasks.
5. HTML mail rendering, rich compose, rules, Quick Steps, search folders.

## Clean room

See `AGENTS.md`. Outlook is only observed black-box; its binaries are never decompiled or read,
and no Microsoft assets or wording are used.

## License

MIT OR Apache-2.0.
