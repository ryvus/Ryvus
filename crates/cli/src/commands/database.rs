use ryvus_execution::StateStoreResult;

use crate::error::{CliError, Result};

pub fn migrate(database_url: Option<String>) -> Result<()> {
    migrate_with(
        database_url,
        std::env::var("DATABASE_URL").ok(),
        ryvus_persistence::migrate,
    )
}

fn migrate_with(
    database_url: Option<String>,
    environment_url: Option<String>,
    migration: impl FnOnce(&str) -> StateStoreResult<()>,
) -> Result<()> {
    let database_url = database_url
        .or(environment_url)
        .filter(|url| !url.trim().is_empty())
        .ok_or(CliError::DatabaseUrlRequired)?;

    migration(&database_url).map_err(|_| CliError::DatabaseMigration)
}

#[cfg(test)]
mod tests {
    use ryvus_execution::StateStoreError;

    use super::*;

    #[test]
    fn flag_takes_precedence_over_environment() {
        migrate_with(
            Some("postgres://flag-secret".into()),
            Some("postgres://environment-secret".into()),
            |url| {
                assert_eq!(url, "postgres://flag-secret");
                Ok(())
            },
        )
        .expect("migration should use the flag URL");
    }

    #[test]
    fn environment_is_used_when_flag_is_absent() {
        migrate_with(None, Some("postgres://environment".into()), |url| {
            assert_eq!(url, "postgres://environment");
            Ok(())
        })
        .expect("migration should use DATABASE_URL");
    }

    #[test]
    fn missing_database_url_is_typed() {
        let error = migrate_with(None, None, |_| Ok(())).expect_err("URL should be required");

        assert!(matches!(error, CliError::DatabaseUrlRequired));
    }

    #[test]
    fn migration_failure_does_not_expose_credentials() {
        let error = migrate_with(
            Some("postgres://user:secret@localhost/ryvus".into()),
            None,
            |_| Err(StateStoreError::Backend("sensitive backend detail".into())),
        )
        .expect_err("migration should fail");
        let message = error.to_string();

        assert!(matches!(error, CliError::DatabaseMigration));
        assert_eq!(message, "database migration failed");
        assert!(!message.contains("secret"));
    }
}
