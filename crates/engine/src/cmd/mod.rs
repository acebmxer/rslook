//! The command registry. Every user-visible action is a [`CommandSpec`]; front ends dispatch by id.

mod account;
mod compose;
mod edit;
mod import;
mod mail;
mod sendrecv;
mod view;

use crate::{EngineError, Session};
use serde_json::Value;

pub type RunFn = fn(&mut Session, &Value) -> Result<(), EngineError>;
pub type EnabledFn = fn(&Session) -> bool;

#[derive(Clone, Copy)]
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Keyboard shortcut in Outlook classic's notation, matched by the UI.
    pub shortcut: Option<&'static str>,
    /// Parameter documentation (for CLI/MCP).
    pub params: &'static str,
    /// Mutates the mailbox and so takes an undo snapshot.
    pub undoable: bool,
    pub enabled: EnabledFn,
    pub run: RunFn,
}

pub(crate) fn always(_: &Session) -> bool {
    true
}

pub(crate) fn has_selection(s: &Session) -> bool {
    s.selected.is_some_and(|id| s.store.message(id).is_some())
}

pub(crate) fn composing(s: &Session) -> bool {
    s.compose.is_some()
}

pub(crate) fn str_param<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}

pub(crate) fn u64_param(p: &Value, key: &str) -> Option<u64> {
    p.get(key).and_then(Value::as_u64)
}

/// Every command, in a stable order.
pub fn commands() -> Vec<CommandSpec> {
    let mut v = Vec::new();
    v.extend_from_slice(mail::COMMANDS);
    v.extend_from_slice(compose::COMMANDS);
    v.extend_from_slice(view::COMMANDS);
    v.extend_from_slice(edit::COMMANDS);
    v.extend_from_slice(sendrecv::COMMANDS);
    v.extend_from_slice(account::COMMANDS);
    v.extend_from_slice(import::COMMANDS);
    v
}

pub fn find(id: &str) -> Option<CommandSpec> {
    commands().into_iter().find(|c| c.id == id)
}
