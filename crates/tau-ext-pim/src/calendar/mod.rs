//! Calendar module for the standard PIM extension.
//!
//! This module establishes the model-visible `calendar` tool surface and
//! extension-owned slash actions. Real backends land in later commits; this
//! initial core keeps configuration, routing, and policy boundaries in place.

mod actions;
mod config;
mod google;
mod ics_feed;
mod runtime;
mod tool;

pub use actions::calendar_action_schema;
pub use config::{
    CalendarAccountConfig, CalendarBackendConfig, CalendarExtensionConfig, CalendarSelectionConfig,
};
pub use google::GoogleBackend;
pub use ics_feed::IcsFeedBackend;
pub use runtime::RuntimeState;
pub use tool::{calendar_prompt_fragment, calendar_tool_spec};

/// Tau-internal and model-visible tool name for calendar commands.
pub const TOOL_NAME: &str = "calendar";
