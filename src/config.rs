use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

use thiserror::Error;
use url::Url;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderKind {
    #[default]
    Postgres,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

impl SslMode {
    fn parse(value: &str) -> Result<Self, ConfigError> {
        match value.to_ascii_lowercase().as_str() {
            "disable" => Ok(Self::Disable),
            "prefer" | "allow" => Ok(Self::Prefer),
            "require" | "verify-ca" | "verify-full" => Ok(Self::Require),
            _ => Err(ConfigError::InvalidSslMode),
        }
    }
}

/// A secret that is always redacted by formatting (FR-033).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostgresConfiguration {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: SecretString,
    pub ssl_mode: SslMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    pub provider: ProviderKind,
    pub configuration: PostgresConfiguration,
}

impl ConnectionProfile {
    pub fn manual(name: impl Into<String>, configuration: PostgresConfiguration) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            provider: ProviderKind::Postgres,
            configuration,
        }
    }

    /// Loads a profile without changing the source file (FR-003).
    pub fn from_env_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content = fs::read_to_string(path).map_err(ConfigError::ReadEnv)?;
        Self::from_env_str(&content)
    }

    pub fn from_env_str(content: &str) -> Result<Self, ConfigError> {
        let values = parse_env(content)?;
        if let Some(database_url) = values.get("DATABASE_URL") {
            return Self::from_database_url(database_url);
        }

        let host = values
            .get("PGHOST")
            .cloned()
            .unwrap_or_else(|| "localhost".into());
        let port = values
            .get("PGPORT")
            .map(|value| value.parse::<u16>().map_err(|_| ConfigError::InvalidPort))
            .transpose()?
            .unwrap_or(5432);
        let database = values
            .get("PGDATABASE")
            .cloned()
            .ok_or(ConfigError::MissingDatabase)?;
        let username = values
            .get("PGUSER")
            .cloned()
            .ok_or(ConfigError::MissingUsername)?;
        let password = values.get("PGPASSWORD").cloned().unwrap_or_default();
        let ssl_mode = values
            .get("PGSSLMODE")
            .map(|value| SslMode::parse(value))
            .transpose()?
            .unwrap_or_default();

        Ok(Self::manual(
            "Environment",
            PostgresConfiguration {
                host,
                port,
                database,
                username,
                password: SecretString::new(password),
                ssl_mode,
            },
        ))
    }

    /// Loads only the recognised PostgreSQL variables from the process environment.
    /// Values remain in memory and are never logged or persisted (FR-003, FR-033).
    pub fn from_process_env() -> Result<Option<Self>, ConfigError> {
        if let Ok(database_url) = std::env::var("DATABASE_URL") {
            return Self::from_database_url(&database_url).map(Some);
        }

        let recognised = [
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGPASSWORD",
            "PGSSLMODE",
        ];
        let values: HashMap<_, _> = recognised
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key, value)))
            .collect();
        if !values.contains_key("PGDATABASE") && !values.contains_key("PGUSER") {
            return Ok(None);
        }

        let host = values
            .get("PGHOST")
            .cloned()
            .unwrap_or_else(|| "localhost".into());
        let port = values
            .get("PGPORT")
            .map(|value| value.parse::<u16>().map_err(|_| ConfigError::InvalidPort))
            .transpose()?
            .unwrap_or(5432);
        let username = values
            .get("PGUSER")
            .cloned()
            .or_else(current_os_username)
            .ok_or(ConfigError::MissingUsername)?;
        let database = values
            .get("PGDATABASE")
            .cloned()
            .unwrap_or_else(|| username.clone());
        let ssl_mode = values
            .get("PGSSLMODE")
            .map(|value| SslMode::parse(value))
            .transpose()?
            .unwrap_or_default();

        Ok(Some(Self::manual(
            "Environment",
            PostgresConfiguration {
                host,
                port,
                database,
                username,
                password: SecretString::new(values.get("PGPASSWORD").cloned().unwrap_or_default()),
                ssl_mode,
            },
        )))
    }

    pub fn from_database_url(value: &str) -> Result<Self, ConfigError> {
        let url = Url::parse(value).map_err(|_| ConfigError::InvalidDatabaseUrl)?;
        if url.scheme() != "postgres" && url.scheme() != "postgresql" {
            return Err(ConfigError::UnsupportedScheme);
        }
        let host = url.host_str().ok_or(ConfigError::MissingHost)?.to_owned();
        let database = url.path().trim_start_matches('/').to_owned();
        if database.is_empty() {
            return Err(ConfigError::MissingDatabase);
        }
        if url.username().is_empty() {
            return Err(ConfigError::MissingUsername);
        }
        let ssl_mode = url
            .query_pairs()
            .find(|(key, _)| key == "sslmode")
            .map(|(_, value)| SslMode::parse(&value))
            .transpose()?
            .unwrap_or_default();

        Ok(Self::manual(
            "Environment",
            PostgresConfiguration {
                host,
                port: url.port().unwrap_or(5432),
                database,
                username: url.username().to_owned(),
                password: SecretString::new(url.password().unwrap_or_default()),
                ssl_mode,
            },
        ))
    }

    /// A safe identity for the toolbar. It can never contain a password.
    pub fn display_identity(&self) -> String {
        format!("{} / {}", self.name, self.configuration.database)
    }
}

pub fn local_profile() -> Option<ConnectionProfile> {
    let username = current_os_username()?;
    Some(ConnectionProfile::manual(
        "Local PostgreSQL",
        PostgresConfiguration {
            host: "localhost".into(),
            port: 5432,
            database: username.clone(),
            username,
            password: SecretString::default(),
            ssl_mode: SslMode::Disable,
        },
    ))
}

fn current_os_username() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read the environment file")]
    ReadEnv(#[source] std::io::Error),
    #[error("invalid environment entry on line {0}")]
    InvalidEnv(usize),
    #[error("DATABASE_URL is not a valid URL")]
    InvalidDatabaseUrl,
    #[error("only postgres and postgresql URLs are supported")]
    UnsupportedScheme,
    #[error("database host is missing")]
    MissingHost,
    #[error("database name is missing")]
    MissingDatabase,
    #[error("database username is missing")]
    MissingUsername,
    #[error("database port is invalid")]
    InvalidPort,
    #[error("PostgreSQL SSL mode is invalid")]
    InvalidSslMode,
}

fn parse_env(content: &str) -> Result<HashMap<String, String>, ConfigError> {
    let mut values = HashMap::new();
    for (index, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, raw_value) = line
            .split_once('=')
            .ok_or(ConfigError::InvalidEnv(index + 1))?;
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
        {
            return Err(ConfigError::InvalidEnv(index + 1));
        }
        values.insert(
            key.to_owned(),
            parse_env_value(raw_value.trim(), index + 1)?,
        );
    }
    Ok(values)
}

fn parse_env_value(value: &str, line: usize) -> Result<String, ConfigError> {
    if let Some(quoted) = value.strip_prefix('"') {
        let inner = quoted
            .strip_suffix('"')
            .ok_or(ConfigError::InvalidEnv(line))?;
        return Ok(inner
            .replace("\\n", "\n")
            .replace("\\r", "\r")
            .replace("\\t", "\t")
            .replace("\\\"", "\"")
            .replace("\\\\", "\\"));
    }
    if let Some(quoted) = value.strip_prefix('\'') {
        return quoted
            .strip_suffix('\'')
            .map(ToOwned::to_owned)
            .ok_or(ConfigError::InvalidEnv(line));
    }
    Ok(value
        .split_once(" #")
        .map_or(value, |(before_comment, _)| before_comment)
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_database_url_without_exposing_password() {
        let profile = ConnectionProfile::from_env_str(
            "DATABASE_URL=postgresql://developer:secret@localhost:5544/example?sslmode=require",
        )
        .unwrap();

        assert_eq!(profile.configuration.host, "localhost");
        assert_eq!(profile.configuration.port, 5544);
        assert_eq!(profile.configuration.database, "example");
        assert_eq!(profile.configuration.username, "developer");
        assert_eq!(profile.configuration.password.expose(), "secret");
        assert_eq!(profile.configuration.ssl_mode, SslMode::Require);
        assert!(!format!("{profile:?}").contains("secret"));
        assert!(!profile.display_identity().contains("secret"));
    }

    #[test]
    fn loads_individual_pg_variables() {
        let profile = ConnectionProfile::from_env_str(
            "PGHOST=db.local\nPGPORT=5433\nPGDATABASE=app\nPGUSER=paul\nPGPASSWORD='secret value'",
        )
        .unwrap();

        assert_eq!(profile.configuration.host, "db.local");
        assert_eq!(profile.configuration.port, 5433);
        assert_eq!(profile.configuration.password.expose(), "secret value");
    }

    #[test]
    fn errors_never_echo_invalid_database_url() {
        let error = ConnectionProfile::from_env_str(
            "DATABASE_URL=postgresql://developer:very-secret@[/example",
        )
        .unwrap_err();

        assert!(!error.to_string().contains("very-secret"));
    }
}
