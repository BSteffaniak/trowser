#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Serializable session model types for trowser.

use serde::{Deserialize, Serialize};

/// Stable identifier for a browser tab in a trowser daemon session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TabId(pub String);

impl TabId {
    /// Creates a new tab id from a string-like value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for a loaded page within a tab.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PageId(pub String);

impl PageId {
    /// Creates a new page id from a string-like value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for an actionable DOM element in a page.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ActionId(pub String);

impl ActionId {
    /// Creates a new action id from a string-like value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Browser tab metadata shared between daemon and client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabSummary {
    /// Tab id assigned by trowser.
    pub id: TabId,
    /// Current page URL.
    pub url: String,
    /// Current page title.
    pub title: String,
    /// Whether this tab is currently selected by the daemon session.
    pub active: bool,
}

impl Default for TabSummary {
    fn default() -> Self {
        Self {
            id: TabId::new("tab-0"),
            url: String::new(),
            title: String::new(),
            active: false,
        }
    }
}

/// Persistable session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSnapshot {
    /// Known tabs in stable display order.
    pub tabs: Vec<TabSummary>,
    /// Active tab id when one exists.
    pub active_tab: Option<TabId>,
}
