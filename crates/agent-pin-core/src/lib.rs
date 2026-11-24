//! Pure core logic for agent-pin: hook input parsing and provider ids.

pub mod input;

pub use input::{display_name, normalize_provider, truncate, HookInput, HookKind};
