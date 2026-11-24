//! Pure core logic for agent-pin: hook input parsing, provider ids and the
//! per-session state machine.

pub mod input;
pub mod session;

pub use input::{display_name, normalize_provider, truncate, HookInput, HookKind};
pub use session::{apply, session_key, CommandRun, FileTouch, SessionState, Status};
