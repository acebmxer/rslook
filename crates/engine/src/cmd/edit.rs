//! Undo and redo.

use super::CommandSpec;
use crate::{EngineError, Session};

pub(super) const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "edit.undo",
        label: "Undo",
        shortcut: Some("Ctrl+Z"),
        params: "",
        undoable: false,
        enabled: Session::can_undo,
        run: |s, _| {
            if s.undo_step() {
                s.status = "Undone".into();
            }
            Ok::<(), EngineError>(())
        },
    },
    CommandSpec {
        id: "edit.redo",
        label: "Redo",
        shortcut: None,
        params: "",
        undoable: false,
        enabled: Session::can_redo,
        run: |s, _| {
            if s.redo_step() {
                s.status = "Redone".into();
            }
            Ok::<(), EngineError>(())
        },
    },
];
