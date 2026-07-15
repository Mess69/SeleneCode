//! The SeleneCode daemon — one warm store per project, shared over a Unix socket.
//!
//! # Why a daemon at all (and why it matters *more* here than in CodeGraph)
//!
//! CodeGraph indexed into SQLite, which is multi-reader: two `serve` processes could each open the
//! DB. SeleneCode's SurrealDB embedded takes an **exclusive** RocksDB lock — a second opener blocks
//! and then fails. And every MCP handler today opens the store *per call* and drops it, so an agent
//! session that makes five tool calls pays five RocksDB opens. The daemon fixes both at once: it is
//! the **sole** process that opens the store, it holds it **warm** for its whole life, and every
//! agent's `serve --mcp` becomes a thin proxy to it. One owner, zero repeated opens.
//!
//! # Shape (POSIX)
//!
//! - [`paths`] — socket / pidfile locations, pure functions of the project root.
//! - [`proc`]  — pid liveness (`kill(pid,0)`), own/parent pid.
//! - [`lock`]  — election via an atomic pidfile-as-lock, stale-corpse compare-and-delete.
//!
//! The serve loop, the launcher/proxy, and the registry build on these. Windows named pipes are a
//! later port (map §Rust port notes); the POSIX path is the one we integration-test.

pub mod control;
pub mod lock;
pub mod paths;
pub mod proc;
pub mod registry;
mod serve;

pub use control::{ControlReply, route_to_daemon};
pub use registry::{DaemonRecord, list as list_daemons};
pub use serve::launch;
