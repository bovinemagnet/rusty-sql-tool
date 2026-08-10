use std::fmt;

use async_trait::async_trait;

use crate::config::ConnectionProfile;
use crate::result::{QueryError, QueryResult};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    View,
    MaterialisedView,
    Function,
    Procedure,
    Sequence,
    Type,
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Table => "Tables",
            Self::View => "Views",
            Self::MaterialisedView => "Materialised Views",
            Self::Function => "Functions",
            Self::Procedure => "Procedures",
            Self::Sequence => "Sequences",
            Self::Type => "Types",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseObject {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub database: String,
    pub server_version: String,
}

/// Database engine boundary required by FR-001 and architectural decision 59.4.
#[async_trait]
pub trait DatabaseProvider: Send + Sync {
    async fn connect(&self, profile: &ConnectionProfile) -> Result<ConnectionInfo, QueryError>;
    async fn disconnect(&self) -> Result<(), QueryError>;
    async fn execute(&self, sql: &str) -> Result<QueryResult, QueryError>;
    async fn cancel(&self) -> Result<(), QueryError>;
    async fn schemas(&self, refresh: bool) -> Result<Vec<String>, QueryError>;
    async fn objects(&self, schema: &str, refresh: bool)
    -> Result<Vec<DatabaseObject>, QueryError>;
    fn state(&self) -> ConnectionState;
}
