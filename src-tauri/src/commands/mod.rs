//! IPC commands exposed to the frontend.
//!
//! Every command follows the same contract (guide §9.1):
//!
//! - `domain_action` naming in `snake_case` — `session_bind`, `message_send`;
//! - `Result<Dto, ErrorDto>` signature, never a `panic!` nor an opaque type;
//! - input validation happens **here**, never in the frontend: the WebView is
//!   treated as untrusted (CLAUDE.md §3);
//! - the `{ code, message, details }` error DTO is stable and leaks neither
//!   file paths nor secrets.
//!
//! A command validates, calls, serialises. Anything longer than that is
//! business logic and belongs in a module or a crate below.

pub(crate) mod config;
pub(crate) mod session;
