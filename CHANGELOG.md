# Changelog

All notable changes to Rslook are listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **IMAP/SMTP accounts with an offline cache.** Add Account and Account Settings on the File
  tab set up one IMAP and SMTP account (SSL/TLS, STARTTLS or, for test servers, no encryption;
  certificates are checked against the system trust store). The password is stored in the OS
  keychain (Secret Service on Linux, Credential Manager on Windows), never in a file. The new
  Send / Receive tab has Send/Receive All Folders (F9), Update Folder (Shift+F9), Send All and
  Work Offline. Sent mail waits in the Outbox, goes out over SMTP and a copy is saved to Sent
  Items. Server folders, read and flagged state, moves and deletes sync both ways. Synced mail
  stays readable offline, and changes made offline are queued, kept across restarts and
  replayed on the next Send/Receive. Without an account the app still opens the sample mailbox.
