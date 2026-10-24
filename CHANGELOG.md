# Changelog

All notable changes to Rslook are listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Import/Export: Outlook data files (.pst) and saved messages (.msg).** File ▸ Import/Export
  takes the path of a `.pst`, `.msg` or `.eml` file (they can also be given on the command
  line). A `.pst` (Unicode or ANSI) is read without being changed and copied into a new folder
  named after the file, with its folder tree inside and each message's sender, recipients,
  date, read and follow-up flags, importance and attachment names. A single `.msg` or `.eml`
  goes into the current folder. Calendar items, contacts, tasks and notes are skipped and
  counted in the status bar; damaged items are left out and counted. Undo removes an import.
  Imported folders stay on this computer: Send/Receive does not upload them. Without an
  account, imports go into the sample mailbox and are not kept after closing.
- **IMAP/SMTP accounts with an offline cache.** Add Account and Account Settings on the File
  tab set up one IMAP and SMTP account (SSL/TLS, STARTTLS or, for test servers, no encryption;
  certificates are checked against the system trust store). The password is stored in the OS
  keychain (Secret Service on Linux, Credential Manager on Windows), never in a file. The new
  Send / Receive tab has Send/Receive All Folders (F9), Update Folder (Shift+F9), Send All and
  Work Offline. Sent mail waits in the Outbox, goes out over SMTP and a copy is saved to Sent
  Items. Server folders, read and flagged state, moves and deletes sync both ways. Synced mail
  stays readable offline, and changes made offline are queued, kept across restarts and
  replayed on the next Send/Receive. Without an account the app still opens the sample mailbox.
