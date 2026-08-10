//! Core application for Rusty SQL Tool.
//!
//! The crate deliberately keeps GPUI and PostgreSQL driver types at the edges. This
//! is the boundary required by FR-021–FR-025 and architectural decisions 59.2–59.4.

pub mod application;
pub mod config;
pub mod database;
pub mod postgres;
pub mod result;
pub mod sql;
pub mod ui;

pub const DEFAULT_ROW_LIMIT: u32 = 10;
pub const MAX_ROW_LIMIT: u32 = 100_000;
