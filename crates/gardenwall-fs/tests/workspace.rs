use std::fs;
use std::path::Path;

#[cfg(windows)]
use gardenwall_fs::EntryKind;
use gardenwall_fs::{Change, CheckpointStore, FsError, Retention, Workspace};
use gardenwall_policy::paths::RelPath;

fn rel(p: &str) -> RelPath {
    RelPath::parse(p).unwrap()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    ws_dir: std::path::PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    outside: std::path::PathBuf,
    store: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let ws_dir = tmp.path().join("project");
    let outside = tmp.path().join("private");
    let store = tmp.path().join("checkpoints");
    for d in [&ws_dir, &outside, &store] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(outside.join("secret.txt"), b"outside data").unwrap();
    fs::write(ws_dir.join("readme.md"), b"hello\n").unwrap();
    Fixture {
        _tmp: tmp,
        ws_dir,
        outside,
        store,
    }
}

#[cfg(windows)]
fn junction(link: &Path, target: &Path) -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn read_write_list_delete_inside_the_workspace() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    assert_eq!(ws.read(&rel("readme.md"), 1024).unwrap(), b"hello\n");
    ws.write(&rel("src/main.rs"), b"fn main() {}\n").unwrap();
    assert_eq!(
        fs::read(f.ws_dir.join("src").join("main.rs")).unwrap(),
        b"fn main() {}\n"
    );
    let names: Vec<_> = ws.list(None).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["readme.md", "src"]);
    // The atomic write leaves no temporary file behind.
    let src: Vec<_> = ws
        .list(Some(&rel("src")))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(src, ["main.rs"]);
    ws.delete(&rel("src/main.rs")).unwrap();
    assert_eq!(ws.kind(&rel("src/main.rs")).unwrap(), None);
    assert!(matches!(
        ws.read(&rel("readme.md"), 3),
        Err(FsError::TooLarge { .. })
    ));
}

#[cfg(windows)]
#[test]
fn junctions_cannot_reach_outside_the_workspace() {
    let f = fixture();
    let link = f.ws_dir.join("escape");
    assert!(junction(&link, &f.outside), "mklink /J failed");
    let ws = Workspace::open(&f.ws_dir).unwrap();

    assert!(ws.read(&rel("escape/secret.txt"), 1024).is_err());
    assert!(ws.write(&rel("escape/planted.txt"), b"x").is_err());
    assert!(ws.delete(&rel("escape/secret.txt")).is_err());
    assert!(ws.kind(&rel("escape/secret.txt")).is_err());
    assert!(ws.list(Some(&rel("escape"))).is_err());
    assert!(!f.outside.join("planted.txt").exists());
    assert_eq!(
        fs::read(f.outside.join("secret.txt")).unwrap(),
        b"outside data"
    );
    // Listings report the junction without following it.
    let entry = ws
        .list(None)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "escape")
        .unwrap();
    assert_eq!(entry.kind, EntryKind::Link);
    // Writing over the junction itself is refused.
    assert!(ws.write(&rel("escape"), b"x").is_err());
}

#[cfg(windows)]
#[test]
fn symlinks_cannot_reach_outside_the_workspace() {
    let f = fixture();
    let link = f.ws_dir.join("leak.txt");
    if std::os::windows::fs::symlink_file(f.outside.join("secret.txt"), &link).is_err() {
        eprintln!("skipping: creating symlinks needs Developer Mode or admin rights");
        return;
    }
    let ws = Workspace::open(&f.ws_dir).unwrap();
    assert!(ws.read(&rel("leak.txt"), 1024).is_err());
    assert!(ws.write(&rel("leak.txt"), b"overwrite").is_err());
    assert_eq!(
        fs::read(f.outside.join("secret.txt")).unwrap(),
        b"outside data"
    );
}

#[test]
fn overly_broad_grants_are_refused() {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .unwrap();
    assert!(matches!(
        Workspace::open(Path::new(&home)),
        Err(FsError::GrantTooBroad(_))
    ));
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    assert!(matches!(
        Workspace::open(Path::new(root)),
        Err(FsError::GrantTooBroad(_))
    ));
    let parent = Path::new(&home).parent().unwrap();
    assert!(matches!(
        Workspace::open(parent),
        Err(FsError::GrantTooBroad(_))
    ));
}

#[test]
fn turns_can_be_reviewed_and_undone_newest_first() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let store = CheckpointStore::open(&f.store, &ws, Retention::default()).unwrap();
    fs::write(f.ws_dir.join("old.txt"), b"delete me\n").unwrap();

    let mut t1 = store.begin_turn("first request").unwrap();
    t1.write(&ws, &rel("readme.md"), b"hello\nworld\n").unwrap();
    t1.write(&ws, &rel("new/file.rs"), b"// new\n").unwrap();
    t1.delete(&ws, &rel("old.txt")).unwrap();
    // A second change to the same file keeps the original snapshot.
    t1.write(&ws, &rel("readme.md"), b"hello\nworld\nagain\n")
        .unwrap();
    assert_eq!(t1.files(), ["readme.md", "new/file.rs", "old.txt"]);

    let mut t2 = store.begin_turn("second request").unwrap();
    t2.write(&ws, &rel("readme.md"), b"rewritten\n").unwrap();

    let diffs = store.diff(&ws, t1.number()).unwrap();
    let changes: Vec<_> = diffs.iter().map(|d| (d.path.as_str(), d.change)).collect();
    assert_eq!(
        changes,
        [
            ("readme.md", Change::Modified),
            ("new/file.rs", Change::Created),
            ("old.txt", Change::Deleted)
        ]
    );

    // Undo goes newest first: turn 2, then turn 1.
    assert_eq!(store.undoable_turn().unwrap(), Some(t2.number()));
    store.undo_latest(&ws).unwrap().unwrap();
    assert_eq!(
        fs::read(f.ws_dir.join("readme.md")).unwrap(),
        b"hello\nworld\nagain\n"
    );
    store.undo_latest(&ws).unwrap().unwrap();
    assert_eq!(fs::read(f.ws_dir.join("readme.md")).unwrap(), b"hello\n");
    assert!(!f.ws_dir.join("new").join("file.rs").exists());
    assert_eq!(fs::read(f.ws_dir.join("old.txt")).unwrap(), b"delete me\n");
    assert_eq!(store.undo_latest(&ws).unwrap(), None);

    let history = store.history().unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.iter().all(|t| t.undone));
    assert_eq!(history[0].label, "second request");
}

#[test]
fn a_single_file_can_be_undone() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let store = CheckpointStore::open(&f.store, &ws, Retention::default()).unwrap();
    let mut t = store.begin_turn("edit").unwrap();
    t.write(&ws, &rel("readme.md"), b"changed\n").unwrap();
    t.write(&ws, &rel("keep.txt"), b"keep\n").unwrap();
    assert!(store.undo_file(&ws, &rel("readme.md")).unwrap());
    assert_eq!(fs::read(f.ws_dir.join("readme.md")).unwrap(), b"hello\n");
    assert_eq!(fs::read(f.ws_dir.join("keep.txt")).unwrap(), b"keep\n");
    assert!(!store.undo_file(&ws, &rel("readme.md")).unwrap());
}

#[test]
fn checkpoints_must_live_outside_the_workspace() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let inside = f.ws_dir.join(".checkpoints");
    fs::create_dir_all(&inside).unwrap();
    assert!(matches!(
        CheckpointStore::open(&inside, &ws, Retention::default()),
        Err(FsError::CheckpointsInsideWorkspace)
    ));
}

#[test]
fn old_turns_are_pruned_by_retention() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let store = CheckpointStore::open(
        &f.store,
        &ws,
        Retention {
            max_turns: 2,
            max_bytes: u64::MAX,
        },
    )
    .unwrap();
    for i in 0..4 {
        let mut t = store.begin_turn(&format!("turn {i}")).unwrap();
        t.write(&ws, &rel("readme.md"), format!("v{i}\n").as_bytes())
            .unwrap();
    }
    assert_eq!(store.prune().unwrap(), 2);
    let labels: Vec<_> = store
        .history()
        .unwrap()
        .into_iter()
        .map(|t| t.label)
        .collect();
    assert_eq!(labels, ["turn 3", "turn 2"]);
}

#[test]
fn a_missing_checkpoint_folder_is_created() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let fresh = f.store.join("not-yet-created").join("checkpoints");
    let store = CheckpointStore::open(&fresh, &ws, Retention::default()).unwrap();
    let mut t = store.begin_turn("first").unwrap();
    t.write(&ws, &rel("a.txt"), b"x").unwrap();
    assert!(fresh.is_dir());
}

#[test]
fn turns_record_the_session_that_made_them() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let store = CheckpointStore::open(&f.store, &ws, Retention::default()).unwrap();
    let mut mine = store.begin_session_turn("mine", Some("session-a")).unwrap();
    mine.write(&ws, &rel("a.txt"), b"a").unwrap();
    let mut other = store.begin_turn("someone else").unwrap();
    other.write(&ws, &rel("b.txt"), b"b").unwrap();
    let history = store.history().unwrap();
    assert_eq!(history[0].session, None);
    assert_eq!(history[1].session.as_deref(), Some("session-a"));
    assert_eq!(history[1].files, vec!["a.txt".to_string()]);
}

#[test]
fn folders_of_secrets_are_never_granted() {
    let tmp = tempfile::tempdir().unwrap();
    for name in [
        ".ssh",
        ".aws/sso",
        "Gardenwall Secrets/shop-1a2b",
        "work/.gnupg",
    ] {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            matches!(Workspace::open(&dir), Err(FsError::SecretStore(_))),
            "{name}"
        );
    }
    // A project that merely contains such a folder is fine: the files in
    // it are hidden by the secret-path rules.
    assert!(Workspace::open(&tmp.path().join("work")).is_ok());
    assert!(gardenwall_fs::secrets_home().is_some());
}

#[test]
fn a_sessions_changes_are_one_diff_per_file_from_before_it_started() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let store = CheckpointStore::open(&f.store, &ws, Retention::default()).unwrap();
    fs::write(f.ws_dir.join("kept.txt"), b"one\n").unwrap();
    fs::write(f.ws_dir.join("back.txt"), b"same\n").unwrap();

    // Two turns of session A, one of session B, one with no session.
    let mut a1 = store.begin_session_turn("a1", Some("A")).unwrap();
    a1.write(&ws, &rel("kept.txt"), b"two\n").unwrap();
    a1.write(&ws, &rel("back.txt"), b"changed\n").unwrap();
    let mut b1 = store.begin_session_turn("b1", Some("B")).unwrap();
    b1.write(&ws, &rel("other.txt"), b"b\n").unwrap();
    let mut a2 = store.begin_session_turn("a2", Some("A")).unwrap();
    a2.write(&ws, &rel("kept.txt"), b"three\n").unwrap();
    a2.write(&ws, &rel("back.txt"), b"same\n").unwrap();
    a2.write(&ws, &rel("made.txt"), b"new\n").unwrap();
    let mut none = store.begin_turn("no session").unwrap();
    none.write(&ws, &rel("loose.txt"), b"x\n").unwrap();

    let diffs = store.session_diff(&ws, "A").unwrap();
    let changes: Vec<_> = diffs.iter().map(|d| (d.path.as_str(), d.change)).collect();
    // `back.txt` ended as it began, so it isn't listed; `kept.txt` is
    // diffed from before the session, not from its second turn.
    assert_eq!(
        changes,
        [
            ("kept.txt", Change::Modified),
            ("made.txt", Change::Created)
        ]
    );
    let kept = diffs[0].unified.as_deref().unwrap();
    assert!(
        kept.contains("-one") && kept.contains("+three") && !kept.contains("two"),
        "{kept}"
    );
    assert_eq!(store.session_diff(&ws, "B").unwrap().len(), 1);
    assert!(store.session_diff(&ws, "nobody").unwrap().is_empty());
}

#[test]
fn a_file_goes_back_to_before_the_session_and_undo_reverses_it() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    let store = CheckpointStore::open(&f.store, &ws, Retention::default()).unwrap();
    fs::write(f.ws_dir.join("kept.txt"), b"one\n").unwrap();
    let mut a1 = store.begin_session_turn("a1", Some("A")).unwrap();
    a1.write(&ws, &rel("kept.txt"), b"two\n").unwrap();
    a1.write(&ws, &rel("made.txt"), b"new\n").unwrap();
    let mut a2 = store.begin_session_turn("a2", Some("A")).unwrap();
    a2.write(&ws, &rel("kept.txt"), b"three\n").unwrap();

    // Another session can't put this session's file back.
    assert!(
        !store
            .revert_session_file(&ws, "B", &rel("kept.txt"))
            .unwrap()
    );
    assert!(
        store
            .revert_session_file(&ws, "A", &rel("kept.txt"))
            .unwrap()
    );
    assert_eq!(fs::read(f.ws_dir.join("kept.txt")).unwrap(), b"one\n");
    // A file the session made is removed.
    assert!(
        store
            .revert_session_file(&ws, "A", &rel("made.txt"))
            .unwrap()
    );
    assert!(!f.ws_dir.join("made.txt").exists());
    assert!(store.session_diff(&ws, "A").unwrap().is_empty());
    // Nothing is left to put back.
    assert!(
        !store
            .revert_session_file(&ws, "A", &rel("kept.txt"))
            .unwrap()
    );
    // Putting back is a turn of its own: undo brings the file back.
    store.undo_latest(&ws).unwrap();
    assert_eq!(fs::read(f.ws_dir.join("made.txt")).unwrap(), b"new\n");
}

/// A file planted where the old fixed temporary name was is never written
/// through (review GW-006), and no temporary file is left behind.
#[test]
fn writes_never_reuse_a_planted_temporary_file() {
    let f = fixture();
    let ws = Workspace::open(&f.ws_dir).unwrap();
    fs::write(f.ws_dir.join(".notes.txt.gardenwall-tmp"), b"planted").unwrap();
    ws.write(&rel("notes.txt"), b"new contents").unwrap();
    assert_eq!(
        fs::read(f.ws_dir.join("notes.txt")).unwrap(),
        b"new contents"
    );
    assert_eq!(
        fs::read(f.ws_dir.join(".notes.txt.gardenwall-tmp")).unwrap(),
        b"planted"
    );
    let leftovers: Vec<_> = fs::read_dir(&f.ws_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".gardenwall-tmp") && n != ".notes.txt.gardenwall-tmp")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
