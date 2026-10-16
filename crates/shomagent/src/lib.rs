//! Shared library for shomagent CLI and shomagentd.

pub mod config;
pub mod daemon;
pub mod llm;
pub mod notify;
pub mod novelty;
pub mod paths;
pub mod protocol;
pub mod rain;
pub mod ring;
pub mod term;

pub use config::{load_crew_defs, MemberDef, RestartPolicy};
pub use notify::{Finding, NotifyLane};
pub use novelty::NoveltyFilter;
pub use protocol::{Request, Response};
pub use ring::RingBuffer;
