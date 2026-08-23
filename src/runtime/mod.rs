//! Everything a run carries: events, ports, pauses, identity, options, time.

mod clock;
mod context;
mod events;
mod identity;
mod options;
mod pause;
mod ports;

#[cfg(feature = "std")]
pub use clock::SystemClock;
pub use clock::{Clock, FixedClock};
pub use context::ExecutionContext;
pub use events::{LogLevel, NodeStatus, RunEvent};
pub use identity::{escape_segment, RunIdentity};
pub use options::{AbortSignal, RunOptions, RunResult};
pub use pause::{Pause, PauseSignal};
pub use ports::Port;
