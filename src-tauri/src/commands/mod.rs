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
//! Empty at milestone 000: the IPC contract and TypeScript type generation
//! land at milestone 001.
