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
        Self::profiles_from_env_file(path)?
            .into_iter()
            .next()
            .ok_or(ConfigError::MissingDatabase)
    }

    pub fn from_env_str(content: &str) -> Result<Self, ConfigError> {
        Self::profiles_from_env_str(content)?
            .into_iter()
            .next()
            .ok_or(ConfigError::MissingDatabase)
    }

    /// Loads every named PostgreSQL profile in a `.env` file.
    ///
    /// The primary pair is `CONNECTION_NAME`/`DATABASE_URL`. Additional connections
    /// use matching suffixes, for example `CONNECTION_NAME_STAGING` and
    /// `DATABASE_URL_STAGING` (FR-003, FR-031).
    pub fn profiles_from_env_file(
        path: impl AsRef<Path>,
    ) -> Result<Vec<ConnectionProfile>, ConfigError> {
        let content = fs::read_to_string(path).map_err(ConfigError::ReadEnv)?;
        Self::profiles_from_env_str(&content)
    }

    /// A file may describe several connections two ways: suffixed keys
    /// (`DATABASE_URL_STAGING`), or repeated `CONNECTION_NAME` blocks. Both are read here, in
    /// document order (FR-003, FR-031).
    pub fn profiles_from_env_str(content: &str) -> Result<Vec<ConnectionProfile>, ConfigError> {
        let mut profiles = Vec::new();
        for block in parse_env_blocks(content)? {
            let from_urls = url_profiles(&block)?;
            if !from_urls.is_empty() {
                profiles.extend(from_urls);
                continue;
            }
            profiles.extend(pg_variable_profile(&block)?);
        }
        Ok(profiles)
    }

    /// Loads only the recognised PostgreSQL variables from the process environment.
    /// Values remain in memory and are never logged or persisted (FR-003, FR-033).
    ///
    /// Suffixed connections are discovered here exactly as they are in a `.env` file, so exporting
    /// `DATABASE_URL_STAGING` in a shell yields the same profiles as writing it to the file.
    pub fn profiles_from_process_env() -> Result<Vec<ConnectionProfile>, ConfigError> {
        let values: HashMap<String, String> = std::env::vars()
            .filter(|(key, _)| is_recognised_key(key))
            .collect();
        let profiles = url_profiles(&values)?;
        if !profiles.is_empty() {
            return Ok(profiles);
        }
        if !values.contains_key("PGDATABASE") && !values.contains_key("PGUSER") {
            return Ok(Vec::new());
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

        Ok(vec![Self::manual(
            values
                .get("CONNECTION_NAME")
                .filter(|name| !name.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| "Environment".into()),
            PostgresConfiguration {
                host,
                port,
                database,
                username,
                password: SecretString::new(values.get("PGPASSWORD").cloned().unwrap_or_default()),
                ssl_mode,
            },
        )])
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

/// Builds a profile from the discrete `PG*` variables of one block, or nothing when the block does
/// not describe a connection at all — a leading block of unrelated keys is not an error.
fn pg_variable_profile(
    values: &HashMap<String, String>,
) -> Result<Option<ConnectionProfile>, ConfigError> {
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
    let database = values
        .get("PGDATABASE")
        .cloned()
        .ok_or(ConfigError::MissingDatabase)?;
    let username = values
        .get("PGUSER")
        .cloned()
        .ok_or(ConfigError::MissingUsername)?;
    let ssl_mode = values
        .get("PGSSLMODE")
        .map(|value| SslMode::parse(value))
        .transpose()?
        .unwrap_or_default();

    Ok(Some(ConnectionProfile::manual(
        values
            .get("CONNECTION_NAME")
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "Environment".into()),
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

/// The keys either configuration source may contribute.
fn is_recognised_key(key: &str) -> bool {
    matches!(
        key,
        "PGHOST"
            | "PGPORT"
            | "PGDATABASE"
            | "PGUSER"
            | "PGPASSWORD"
            | "PGSSLMODE"
            | "CONNECTION_NAME"
            | "DATABASE_URL"
    ) || key.starts_with("DATABASE_URL_")
        || key.starts_with("CONNECTION_NAME_")
}

/// Builds the URL-based profiles a key/value set describes, newest naming scheme first.
///
/// `DATABASE_URL` is unambiguously ours, so an unusable value there is an error. The suffixed
/// siblings share a namespace with other tooling — the Docker-secrets idiom `DATABASE_URL_FILE`,
/// or `DATABASE_URL_PRISMA` pointing at another engine — so one that is not a PostgreSQL URL is
/// skipped rather than discarding every connection that did parse.
fn url_profiles(values: &HashMap<String, String>) -> Result<Vec<ConnectionProfile>, ConfigError> {
    let mut profiles = Vec::new();
    if let Some(database_url) = values.get("DATABASE_URL") {
        profiles.push(named_url_profile(
            database_url,
            values.get("CONNECTION_NAME").map(String::as_str),
            "Environment",
        )?);
    }

    let mut additional_urls: Vec<_> = values
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("DATABASE_URL_")
                .filter(|suffix| !suffix.is_empty())
                .map(|suffix| (suffix, value))
        })
        .collect();
    additional_urls.sort_by_key(|(suffix, _)| *suffix);
    for (suffix, database_url) in additional_urls {
        let name_key = format!("CONNECTION_NAME_{suffix}");
        if let Ok(profile) = named_url_profile(
            database_url,
            values.get(&name_key).map(String::as_str),
            &connection_name_from_suffix(suffix),
        ) {
            profiles.push(profile);
        }
    }
    Ok(profiles)
}

fn named_url_profile(
    database_url: &str,
    name: Option<&str>,
    fallback_name: &str,
) -> Result<ConnectionProfile, ConfigError> {
    let mut profile = ConnectionProfile::from_database_url(database_url)?;
    profile.name = name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(fallback_name)
        .to_owned();
    Ok(profile)
}

fn connection_name_from_suffix(suffix: &str) -> String {
    suffix
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + &characters.as_str().to_ascii_lowercase()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    #[error("duplicate key on line {0} would silently replace an earlier connection setting")]
    DuplicateKey(usize),
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

/// Splits a `.env` into connection blocks.
///
/// A second bare `CONNECTION_NAME` starts a new block, so one file can describe several
/// connections as repeated blocks of `PG*` variables. Suffixed keys such as
/// `CONNECTION_NAME_STAGING` are not block boundaries and stay with the block they appear in, which
/// leaves the suffix scheme working exactly as before.
///
/// A key repeated *within* one block is rejected rather than silently overwritten: that ambiguity
/// is what previously let a multi-connection file collapse into one mislabelled profile.
fn parse_env_blocks(content: &str) -> Result<Vec<HashMap<String, String>>, ConfigError> {
    let mut blocks: Vec<HashMap<String, String>> = vec![HashMap::new()];
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
        let value = parse_env_value(raw_value.trim(), index + 1)?;

        let starts_new_block = key == "CONNECTION_NAME"
            && blocks
                .last()
                .is_some_and(|block| block.contains_key("CONNECTION_NAME"));
        if starts_new_block {
            blocks.push(HashMap::new());
        }
        let block = blocks
            .last_mut()
            .expect("a block is always open for insertion");
        if block.insert(key.to_owned(), value).is_some() {
            return Err(ConfigError::DuplicateKey(index + 1));
        }
    }
    Ok(blocks)
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
            "CONNECTION_NAME=Development\nDATABASE_URL=postgresql://developer:secret@localhost:5544/example?sslmode=require",
        )
        .unwrap();

        assert_eq!(profile.name, "Development");
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

    #[test]
    fn loads_multiple_named_database_urls_in_stable_order() {
        let profiles = ConnectionProfile::profiles_from_env_str(
            "CONNECTION_NAME=Local Paul\n\
             DATABASE_URL=postgresql://paul@localhost:5432/paul?sslmode=disable\n\
             DATABASE_URL_PRODUCTION=postgresql://reader@prod.example:5432/app\n\
             CONNECTION_NAME_PRODUCTION=Production Read Only\n\
             DATABASE_URL_STAGING_EU=postgresql://reader@staging.example:5432/app",
        )
        .unwrap();

        assert_eq!(profiles.len(), 3);
        assert_eq!(profiles[0].display_identity(), "Local Paul / paul");
        assert_eq!(profiles[1].name, "Production Read Only");
        assert_eq!(profiles[2].name, "Staging Eu");
        assert_eq!(profiles[2].configuration.host, "staging.example");
    }

    /// Other tooling writes keys into the `DATABASE_URL_*` namespace. One of those must not take
    /// every real connection down with it.
    #[test]
    fn unusable_sibling_url_keys_do_not_discard_the_configured_connections() {
        let profiles = ConnectionProfile::profiles_from_env_str(
            "DATABASE_URL=postgresql://dev@localhost:5432/app\n\
             DATABASE_URL_FILE=/run/secrets/db_url\n\
             DATABASE_URL_PRISMA=mysql://root@localhost:3306/app\n\
             DATABASE_URL_STAGING=postgresql://reader@staging.example:5432/app",
        )
        .unwrap();

        let names: Vec<_> = profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect();
        assert_eq!(names, ["Environment", "Staging"]);
    }

    /// A file may name its connections as repeated `CONNECTION_NAME` blocks rather than with
    /// suffixes. Each block is its own connection, in document order.
    #[test]
    fn repeated_connection_name_blocks_each_become_a_connection() {
        let profiles = ConnectionProfile::profiles_from_env_str(
            "CONNECTION_NAME=Local Development\n\
             DATABASE_URL=postgresql://alice@localhost:5432/localdb\n\
             \n\
             CONNECTION_NAME=Staging\n\
             PGHOST=staging.example\n\
             PGPORT=5432\n\
             PGDATABASE=stagingdb\n\
             PGUSER=bob\n\
             PGSSLMODE=require\n\
             \n\
             CONNECTION_NAME=Production Read Only\n\
             PGHOST=prod.example\n\
             PGPORT=5432\n\
             PGDATABASE=proddb\n\
             PGUSER=carol\n\
             PGSSLMODE=require",
        )
        .unwrap();

        assert_eq!(profiles.len(), 3);
        assert_eq!(
            profiles[0].display_identity(),
            "Local Development / localdb"
        );
        assert_eq!(profiles[1].display_identity(), "Staging / stagingdb");
        assert_eq!(profiles[1].configuration.host, "staging.example");
        assert_eq!(
            profiles[2].display_identity(),
            "Production Read Only / proddb"
        );
        assert_eq!(profiles[2].configuration.host, "prod.example");
        assert_eq!(profiles[2].configuration.ssl_mode, SslMode::Require);
    }

    /// The specific way this used to fail: every block collapsed into one profile wearing the last
    /// block's name and the first block's target, so a row labelled "Production" pointed at local.
    #[test]
    fn a_connection_never_wears_another_blocks_name() {
        let profiles = ConnectionProfile::profiles_from_env_str(
            "CONNECTION_NAME=Local Development\n\
             DATABASE_URL=postgresql://alice@localhost:5432/localdb\n\
             CONNECTION_NAME=Production Read Only\n\
             PGHOST=prod.example\n\
             PGDATABASE=proddb\n\
             PGUSER=carol",
        )
        .unwrap();

        let local = &profiles[0];
        assert_eq!(local.name, "Local Development");
        assert_eq!(local.configuration.host, "localhost");

        let production = &profiles[1];
        assert_eq!(production.name, "Production Read Only");
        assert_ne!(production.configuration.host, "localhost");
        assert_eq!(production.configuration.database, "proddb");
    }

    /// Repeating a key inside one block is ambiguous, and silently keeping the last value is how
    /// connections went missing. It is now a hard error naming the line.
    #[test]
    fn a_key_repeated_within_one_block_is_rejected() {
        let error = ConnectionProfile::profiles_from_env_str(
            "CONNECTION_NAME=Local\n\
             PGDATABASE=first\n\
             PGUSER=alice\n\
             PGDATABASE=second",
        )
        .unwrap_err();

        assert!(matches!(error, ConfigError::DuplicateKey(4)));
    }

    /// Keys appearing before any `CONNECTION_NAME` are not a connection of their own.
    #[test]
    fn a_leading_block_without_connection_keys_is_not_a_connection() {
        let profiles = ConnectionProfile::profiles_from_env_str(
            "PGSSLMODE=require\n\
             CONNECTION_NAME=Local\n\
             PGDATABASE=localdb\n\
             PGUSER=alice",
        )
        .unwrap();

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Local");
    }

    /// An unusable primary `DATABASE_URL` is still an error — it is unambiguously ours.
    #[test]
    fn unusable_primary_database_url_is_still_an_error() {
        let error =
            ConnectionProfile::profiles_from_env_str("DATABASE_URL=mysql://root@localhost/app")
                .unwrap_err();

        assert!(matches!(error, ConfigError::UnsupportedScheme));
    }

    /// Both configuration sources run the same suffix scan, so exporting the variables in a shell
    /// discovers exactly what writing them to `.env` would.
    #[test]
    fn suffix_discovery_is_shared_by_both_configuration_sources() {
        let values = HashMap::from([
            (
                "DATABASE_URL".to_owned(),
                "postgresql://dev@localhost:5432/app".to_owned(),
            ),
            (
                "DATABASE_URL_STAGING".to_owned(),
                "postgresql://reader@staging.example:5432/app".to_owned(),
            ),
            ("CONNECTION_NAME_STAGING".to_owned(), "Staging".to_owned()),
        ]);

        let profiles = url_profiles(&values).unwrap();

        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[1].name, "Staging");
        assert!(is_recognised_key("DATABASE_URL_STAGING"));
        assert!(is_recognised_key("CONNECTION_NAME_STAGING"));
        assert!(!is_recognised_key("PATH"));
    }
}
