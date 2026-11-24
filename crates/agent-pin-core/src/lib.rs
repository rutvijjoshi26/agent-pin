//! Pure core logic for agent-pin: hook input parsing, provider ids and the
//! per-session state machine and the on-disk session store.

pub mod hook;
pub mod install;
pub mod input;
pub mod rows;
pub mod session;
pub mod store;

pub use hook::run_hook;
pub use input::{display_name, normalize_provider, truncate, HookInput, HookKind};
pub use rows::{build_rows, CatchUp, Collision, Row};
pub use session::{apply, session_key, CommandRun, FileTouch, SessionState, Status};
pub use store::{list_sessions, mark_seen, reset, state_dir, update_session};
