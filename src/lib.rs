//! casimir — replay, rerun and compare coding-agent sessions (Claude Code, Codex).
pub mod adapters;
pub mod brief;
pub mod cli;
pub mod compare;
pub mod llm;
pub mod model;
pub mod pairs;
pub mod play;
pub mod render;
pub mod rerun;
pub mod simulate;
pub mod util;
pub mod workspace;

pub mod process;
pub mod doctor;
pub mod checkpoint;
pub mod recovery;
pub mod checks;
pub mod privacy;
pub mod artifacts;
