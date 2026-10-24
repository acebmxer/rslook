//! Send / Receive tab commands. They only queue requests; a front end runs the jobs (see
//! [`crate::sync`]).

use super::CommandSpec;
use super::account::has_account;
use crate::sync::SyncRequest;
use crate::{EngineError, Session};
use serde_json::Value;

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "sendrecv.all",
        label: "Send/Receive All Folders",
        shortcut: Some("F9"),
        params: "",
        undoable: false,
        enabled: online,
        run: |s, _| queue(s, SyncRequest::All),
    },
    CommandSpec {
        id: "sendrecv.update_folder",
        label: "Update Folder",
        shortcut: Some("Shift+F9"),
        params: "",
        undoable: false,
        enabled: online,
        run: update_folder,
    },
    CommandSpec {
        id: "sendrecv.send_all",
        label: "Send All",
        shortcut: None,
        params: "",
        undoable: false,
        enabled: online,
        run: |s, _| queue(s, SyncRequest::SendAll),
    },
    CommandSpec {
        id: "sendrecv.work_offline",
        label: "Work Offline",
        shortcut: None,
        params: "",
        undoable: false,
        enabled: has_account,
        run: work_offline,
    },
];

fn online(s: &Session) -> bool {
    has_account(s) && !s.work_offline
}

fn queue(s: &mut Session, r: SyncRequest) -> Result<(), EngineError> {
    s.request(r);
    Ok(())
}

fn update_folder(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    let f = s.folder;
    queue(s, SyncRequest::Folder(f))
}

fn work_offline(s: &mut Session, _: &Value) -> Result<(), EngineError> {
    s.work_offline = !s.work_offline;
    if s.work_offline {
        s.requests.clear();
        s.status = "Working offline".into();
    } else {
        s.status = String::new();
        s.request(SyncRequest::All);
    }
    Ok(())
}
