//! A sample mailbox (our own fictional people and text) for first run, demos and screenshots.

use crate::Session;
use chrono::{Duration, TimeZone, Utc};
use rslook_mail::{Address, Attachment, FolderKind, Importance, NewMessage, Store};

pub fn sample_store() -> Store {
    let me = Address::new("Alex Morgan", "alex@rslook.example");
    let mut s = Store::new(me.clone());
    let base = Utc.with_ymd_and_hms(2026, 10, 23, 16, 45, 0).single().unwrap_or_else(Utc::now);
    let inbox = s.folder_of_kind(FolderKind::Inbox).unwrap_or(0);
    let sent = s.folder_of_kind(FolderKind::Sent).unwrap_or(0);
    let projects = s.add_folder("Projects", FolderKind::Custom, Some(inbox));
    let _ = s.add_folder("Receipts", FolderKind::Custom, Some(inbox));

    let items: [(u64, &str, &str, &str, &str, i64, bool, bool, Importance, Vec<Attachment>); 9] = [
        (
            inbox,
            "Priya Natarajan",
            "priya@lumen.example",
            "Q4 roadmap review — slides attached",
            "Hi Alex,\n\nAttached are the slides for Monday's roadmap review. Could you look over the sync section before then?\n\nThanks,\nPriya",
            0,
            false,
            true,
            Importance::High,
            vec![Attachment { name: "Q4-roadmap.pdf".into(), size: 2_310_000 }],
        ),
        (
            inbox,
            "Marcus Hale",
            "marcus@harbor.example",
            "Lunch Thursday?",
            "Are you free for lunch on Thursday? The new noodle place on 5th opened this week.\n\nMarcus",
            -2,
            false,
            false,
            Importance::Normal,
            vec![],
        ),
        (
            inbox,
            "Build Server",
            "ci@builds.example",
            "Nightly build #4182 passed",
            "All 1,204 tests passed in 6m 12s.\n\nArtifacts: linux-x64, windows-x64.",
            -5,
            true,
            false,
            Importance::Low,
            vec![],
        ),
        (
            inbox,
            "Elena Ruiz",
            "elena@northwind.example",
            "Contract draft v3",
            "Alex,\n\nHere is version 3 of the contract with the changes from Tuesday's call. Legal signed off on sections 2 through 5.\n\nBest,\nElena",
            -26,
            true,
            false,
            Importance::Normal,
            vec![Attachment { name: "contract-v3.docx".into(), size: 88_400 }],
        ),
        (
            inbox,
            "Community Garden",
            "news@garden.example",
            "October newsletter: bulb planting day",
            "Join us Saturday at 10 for bulb planting. Gloves and coffee provided.",
            -50,
            true,
            false,
            Importance::Normal,
            vec![],
        ),
        (
            inbox,
            "Jordan Kim",
            "jordan@lumen.example",
            "Re: offline mode design",
            "I think we should store the local cache per account so a broken account never blocks the others. Thoughts?",
            -74,
            true,
            true,
            Importance::Normal,
            vec![],
        ),
        (
            projects,
            "Priya Natarajan",
            "priya@lumen.example",
            "PST import milestones",
            "Milestone 1: read the header and node B-tree. Milestone 2: folders. Milestone 3: messages and attachments.",
            -120,
            true,
            false,
            Importance::Normal,
            vec![],
        ),
        (sent, "Alex Morgan", "alex@rslook.example", "Re: Lunch Thursday?", "Thursday works. Noon?", -1, true, false, Importance::Normal, vec![]),
        (
            sent,
            "Alex Morgan",
            "alex@rslook.example",
            "Weekly status",
            "Shipped: three-pane layout. Next: IMAP sync and the PST reader.",
            -30,
            true,
            false,
            Importance::Normal,
            vec![],
        ),
    ];
    for (folder, name, email, subject, body, hours, read, flagged, importance, attachments) in items {
        let to = if folder == sent { vec![Address::new("Marcus Hale", "marcus@harbor.example")] } else { vec![me.clone()] };
        let _ = s.add_message(
            folder,
            NewMessage {
                from: Address::new(name, email),
                to,
                cc: vec![],
                subject: subject.into(),
                date: Some(base + Duration::hours(hours)),
                body: body.into(),
                read,
                flagged,
                importance,
                attachments,
            },
        );
    }
    s
}

pub fn sample_session() -> Session {
    Session::new(sample_store())
}
