# Changelog

All notable changes to Rslook are listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **Microsoft 365 and Outlook.com accounts.** Add Account has a new account type, Microsoft
  365 / Outlook.com: you sign in on Microsoft's page in your web browser and Rslook keeps only
  the sign-in token, in the OS keychain (it never sees the password). Send/Receive then works
  through Microsoft Graph, or through IMAP and SMTP with OAuth sign-in if you choose that in the
  dialog: folders, mail, read and flagged state, moves, deletes and sending, with the same
  offline cache and queue as IMAP accounts. Rslook ships no app ID of its own: signing in needs
  the application (client) ID of your own app registration in Microsoft Entra (steps in the
  README), entered in the account dialog. When Microsoft stops accepting
  the saved sign-in, Send/Receive says so and Account Settings lets you sign in again.
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
