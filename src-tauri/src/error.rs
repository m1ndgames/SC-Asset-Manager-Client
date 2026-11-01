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
}

impl AppError {
    /// Stable discriminant for the frontend.
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::LogNotFound => "logNotFound",
            AppError::LogUnreadable { .. } => "logUnreadable",
            AppError::Config(_) => "config",
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("AppError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
