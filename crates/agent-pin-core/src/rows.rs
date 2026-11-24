//! Turns stored sessions into the rows the overlay shows: visibility,
//! sort order, the catch-up card and cross-session file collisions.

use serde::Serialize;

use crate::input::display_name;
use crate::session::{CommandRun, SessionState, Status};

/// Completed and Error rows stay this long after the user has seen them.
pub const COMPLETED_VISIBLE_AFTER_SEEN_MS: i64 = 600_000;
/// Idle rows are shown only this long after their last update.
pub const IDLE_VISIBLE_MS: i64 = 120_000;
/// A Running row with no update for longer than this is marked quiet.
pub const RUNNING_QUIET_MS: i64 = 1_800_000;
/// Two sessions editing the same path within this window collide.
pub const COLLISION_WINDOW_MS: i64 = 600_000;

/// What happened in a session since the user last looked at it.
#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct CatchUp {
    pub original_ask: Option<String>,
    pub current_ask: Option<String>,
    pub files_since_seen: Vec<String>,
    pub commands_since_seen: Vec<CommandRun>,
    pub unseen: bool,
}

/// Another session recently edited a path this session also edited.
#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct Collision {
    pub path: String,
    pub other_label: String,
}

/// One overlay row per agent session.
#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub key: String,
    pub label: String,
    pub project: Option<String>,
    pub status: Status,
    pub quiet: bool,
    pub updated_ms: i64,
    pub catch_up: CatchUp,
    pub collisions: Vec<Collision>,
}

/// Builds the visible rows, sorted for display.
pub fn build_rows(sessions: &[SessionState], now_ms: i64) -> Vec<Row> {
    let mut rows: Vec<Row> = sessions
        .iter()
        .filter(|s| visible(s, now_ms))
        .map(|s| Row {
            key: row_key(s),
            label: label(s),
            project: project(s.cwd.as_deref()),
            status: s.status,
            quiet: s.status == Status::Running && now_ms - s.updated_ms > RUNNING_QUIET_MS,
            updated_ms: s.updated_ms,
            catch_up: catch_up(s),
            collisions: collisions(s, sessions, now_ms),
        })
        .collect();
    rows.sort_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then(b.updated_ms.cmp(&a.updated_ms))
    });
    rows
}

fn visible(s: &SessionState, now_ms: i64) -> bool {
    match s.status {
        Status::WaitingApproval | Status::Running => true,
        Status::Error | Status::Completed => {
            unseen(s) || now_ms - s.seen_ms < COMPLETED_VISIBLE_AFTER_SEEN_MS
        }
        Status::Idle => now_ms - s.updated_ms < IDLE_VISIBLE_MS,
    }
}

fn rank(status: Status) -> u8 {
    match status {
        Status::WaitingApproval => 0,
        Status::Error => 1,
        Status::Running => 2,
        Status::Completed => 3,
        Status::Idle => 4,
    }
}

fn unseen(s: &SessionState) -> bool {
    s.updated_ms > s.seen_ms
}

fn row_key(s: &SessionState) -> String {
    format!("{}__{}", s.provider, s.session_key)
}

/// `"Claude · web-app"`, or just `"Claude"` without a project.
fn label(s: &SessionState) -> String {
    let name = display_name(&s.provider);
    match project(s.cwd.as_deref()) {
        Some(project) => format!("{name} · {project}"),
        None => name,
    }
}

/// The cwd's last component, splitting on both `/` and `\`.
fn project(cwd: Option<&str>) -> Option<String> {
    cwd?
        .split(['/', '\\'])
        .rfind(|part| !part.is_empty())
        .map(str::to_owned)
}

fn catch_up(s: &SessionState) -> CatchUp {
    let mut files_since_seen: Vec<String> = Vec::new();
    for f in s.files.iter().filter(|f| f.at_ms > s.seen_ms) {
        if !files_since_seen.contains(&f.path) {
            files_since_seen.push(f.path.clone());
        }
    }
    CatchUp {
        original_ask: s.original_ask.clone(),
        current_ask: s.pending.clone(),
        files_since_seen,
        commands_since_seen: s
            .commands
            .iter()
            .filter(|c| c.at_ms > s.seen_ms)
            .cloned()
            .collect(),
        unseen: unseen(s),
    }
}

/// Paths `s` touched within the window that another session also touched
/// within the window. Paths are compared as plain strings.
fn collisions(s: &SessionState, all: &[SessionState], now_ms: i64) -> Vec<Collision> {
    let recent = |s: &SessionState, path: &str| {
        s.files
            .iter()
            .any(|f| f.path == path && now_ms - f.at_ms < COLLISION_WINDOW_MS)
    };
    let me = row_key(s);
    let mut out: Vec<Collision> = Vec::new();
    for f in &s.files {
        if !recent(s, &f.path) {
            continue;
        }
        for other in all.iter().filter(|o| row_key(o) != me) {
            if recent(other, &f.path) {
                let c = Collision { path: f.path.clone(), other_label: label(other) };
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::FileTouch;

    const MIN: i64 = 60_000;
    const NOW: i64 = 10_000_000_000;

    fn session(id: &str, status: Status, updated_ms: i64) -> SessionState {
        SessionState {
            provider: "claude".to_owned(),
            session_key: id.to_owned(),
            cwd: Some("/r".to_owned()),
            status,
            updated_ms,
            seen_ms: 0,
            original_ask: None,
            last_ask: None,
            pending: None,
            files: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn keys(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.key.as_str()).collect()
    }

    fn touch(path: &str, at_ms: i64) -> FileTouch {
        FileTouch { path: path.to_owned(), at_ms }
    }

    fn run(command: &str, at_ms: i64) -> CommandRun {
        CommandRun { command: command.to_owned(), ok: true, at_ms }
    }

    #[test]
    fn waiting_never_expires() {
        let s = session("w", Status::WaitingApproval, NOW - 3 * 24 * 60 * MIN);
        let rows = build_rows(&[s], NOW);
        assert_eq!(keys(&rows), ["claude__w"]);
        assert!(!rows[0].quiet);
    }

    #[test]
    fn sort_order_waiting_first() {
        let sessions = [
            session("idle", Status::Idle, NOW - 1),
            session("done", Status::Completed, NOW - 2),
            session("run_old", Status::Running, NOW - 50),
            session("run_new", Status::Running, NOW - 3),
            session("err", Status::Error, NOW - 4),
            session("wait", Status::WaitingApproval, NOW - 100),
        ];
        let rows = build_rows(&sessions, NOW);
        assert_eq!(
            keys(&rows),
            [
                "claude__wait",
                "claude__err",
                "claude__run_new",
                "claude__run_old",
                "claude__done",
                "claude__idle"
            ]
        );
    }

    #[test]
    fn completed_hidden_ten_minutes_after_seen() {
        for status in [Status::Completed, Status::Error] {
            let mut s = session("c", status, NOW - 20 * MIN);
            s.seen_ms = NOW - 10 * MIN;
            assert!(build_rows(std::slice::from_ref(&s), NOW).is_empty());
            s.seen_ms = NOW - 10 * MIN + 1;
            assert_eq!(build_rows(&[s], NOW).len(), 1);
        }
    }

    #[test]
    fn completed_kept_while_unseen() {
        for status in [Status::Completed, Status::Error] {
            let mut s = session("c", status, NOW - 5 * 24 * 60 * MIN);
            s.seen_ms = s.updated_ms - 1;
            let rows = build_rows(&[s], NOW);
            assert_eq!(rows.len(), 1);
            assert!(rows[0].catch_up.unseen);
        }
    }

    #[test]
    fn idle_hidden_after_two_minutes() {
        let shown = session("a", Status::Idle, NOW - 2 * MIN + 1);
        let hidden = session("b", Status::Idle, NOW - 2 * MIN);
        assert_eq!(keys(&build_rows(&[shown, hidden], NOW)), ["claude__a"]);
    }

    #[test]
    fn running_marked_quiet_after_30_minutes() {
        let busy = session("busy", Status::Running, NOW - 30 * MIN);
        let quiet = session("quiet", Status::Running, NOW - 30 * MIN - 1);
        let rows = build_rows(&[busy, quiet], NOW);
        assert_eq!(keys(&rows), ["claude__busy", "claude__quiet"]);
        assert!(!rows[0].quiet);
        assert!(rows[1].quiet);
    }

    #[test]
    fn catch_up_counts_only_since_seen() {
        let mut s = session("c", Status::Running, NOW);
        s.seen_ms = 100;
        s.original_ask = Some("build it".to_owned());
        s.last_ask = Some("fix tests".to_owned());
        s.pending = Some("Bash: rm -rf build".to_owned());
        s.files = vec![
            touch("/r/old.rs", 100),
            touch("/r/a.rs", 101),
            touch("/r/b.rs", 102),
            touch("/r/a.rs", 103),
        ];
        s.commands = vec![run("old", 50), run("new", 200)];
        let c = &build_rows(&[s], NOW)[0].catch_up;
        assert_eq!(c.original_ask.as_deref(), Some("build it"));
        assert_eq!(c.current_ask.as_deref(), Some("Bash: rm -rf build"));
        assert_eq!(c.files_since_seen, ["/r/a.rs", "/r/b.rs"]);
        assert_eq!(c.commands_since_seen, [run("new", 200)]);
        assert!(c.unseen);
    }

    #[test]
    fn collision_reported_on_both_sessions() {
        let mut a = session("a", Status::Running, NOW);
        a.files = vec![touch("/r/a.rs", NOW - 5 * MIN)];
        let mut b = session("b", Status::Running, NOW - 1);
        b.provider = "codex".to_owned();
        b.cwd = Some("/r/sub".to_owned());
        b.files = vec![touch("/r/a.rs", NOW - MIN)];
        let rows = build_rows(&[a, b], NOW);
        assert_eq!(
            rows[0].collisions,
            [Collision { path: "/r/a.rs".to_owned(), other_label: "Codex · sub".to_owned() }]
        );
        assert_eq!(
            rows[1].collisions,
            [Collision { path: "/r/a.rs".to_owned(), other_label: "Claude · r".to_owned() }]
        );
    }

    #[test]
    fn no_collision_outside_window_or_same_session() {
        let mut a = session("a", Status::Running, NOW);
        a.files = vec![touch("/r/a.rs", NOW - MIN), touch("/r/a.rs", NOW - 2 * MIN)];
        let mut b = session("b", Status::Running, NOW);
        b.files = vec![touch("/r/a.rs", NOW - COLLISION_WINDOW_MS)];
        let rows = build_rows(&[a, b], NOW);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.collisions.is_empty()));
    }

    #[test]
    fn label_uses_project_basename() {
        let mut s = session("w", Status::Running, NOW);
        s.cwd = Some("C:\\code\\web-app".to_owned());
        let row = &build_rows(&[s], NOW)[0];
        assert_eq!(row.label, "Claude · web-app");
        assert_eq!(row.project.as_deref(), Some("web-app"));
        assert_eq!(row.key, "claude__w");
    }

    #[test]
    fn label_without_cwd_is_display_name() {
        let mut s = session("w", Status::Running, NOW);
        s.cwd = None;
        let row = &build_rows(&[s], NOW)[0];
        assert_eq!(row.label, "Claude");
        assert_eq!(row.project, None);
        assert_eq!(project(Some("/home/me/repo/")), Some("repo".to_owned()));
    }
}
