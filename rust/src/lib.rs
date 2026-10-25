//! Safe Rust implementation of the vis editor core.
//!
//! Text offsets are bytes, including for files that are not valid UTF-8.
//! See `rust/PORTING.md` for the migration status and compatibility gates.
#![forbid(unsafe_code)]

pub mod digraph;
pub mod editor;
pub mod file;
mod history;
pub mod jumplist;
pub mod motion;
pub mod regex;
pub mod sam;
pub mod selection;
pub mod style;
pub mod text;
pub mod view;
pub mod workspace;

#[cfg(feature = "lua")]
pub mod lua;

#[cfg(all(unix, feature = "terminal"))]
pub mod terminal;
