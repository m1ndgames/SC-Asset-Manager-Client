//! Typed, serialisable errors.
//!
//! `thiserror`, not `anyhow`: a Tauri command must return something the
//! frontend can discriminate on, not an opaque string. The `kind` tag is what
//! the UI switches on.

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("no Game.log found; set the path in Settings")]
    LogNotFound,

    #[error("cannot read {path}: {source}")]
    LogUnreadable {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Config(String),

    /// Reference data could not be fetched or read. Never fatal: the app falls
    /// back to the cached copy, or to asking the user to name the item.
    #[error("{0}")]
    Refdata(String),

    #[error("{0}")]
    Queue(String),

    /// An approval was refused because something is still missing. Carries the
    /// specific blockers so the card can point at the offending field instead
    /// of just saying no.
    #[error("not ready to write: {}", describe_blockers(blockers))]
    NotReady { blockers: Vec<crate::queue::Blocker> },

    /// An update could not be checked for or installed. Never fatal — the app
    /// is perfectly usable without updating.
    #[error("{0}")]
    Update(String),
}

impl AppError {
    /// Stable discriminant for the frontend.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::LogNotFound => "logNotFound",
            AppError::LogUnreadable { .. } => "logUnreadable",
            AppError::Config(_) => "config",
            AppError::Refdata(_) => "refdata",
            AppError::Queue(_) => "queue",
            AppError::NotReady { .. } => "notReady",
            AppError::Update(_) => "update",
        }
    }
}

fn describe_blockers(blockers: &[crate::queue::Blocker]) -> String {
    use crate::queue::Blocker;
    let parts: Vec<&str> = blockers
        .iter()
        .map(|b| match b {
            Blocker::LocationMissing => "the location has not been set",
            Blocker::ItemUnresolved { .. } => "the item could not be identified",
            Blocker::AmountInvalid => "the amount is not a positive number",
            Blocker::PriceInvalid => "the unit price is not valid",
        })
        .collect();
    if parts.is_empty() {
        "nothing".into()
    } else {
        parts.join("; ")
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("AppError", 3)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        // Only `notReady` carries structured detail; the field is always
        // present so the frontend can destructure without a guard.
        match self {
            AppError::NotReady { blockers } => st.serialize_field("blockers", blockers)?,
            _ => st.serialize_field("blockers", &Vec::<crate::queue::Blocker>::new())?,
        }
        st.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
