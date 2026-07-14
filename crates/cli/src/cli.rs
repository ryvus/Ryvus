use std::fmt::Display;

use clap::Parser;
use clap::Subcommand;

#[derive(Parser)]
#[command(name = "ryvus")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    New {
        project_name: String,
        #[arg(long, short, default_value = "python")]
        language: Language,
    },
    Discover,
    Start {
        #[arg(long)]
        schedules: bool,
    },
    Validate,
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    Database {
        #[command(subcommand)]
        command: DatabaseCommand,
    },
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    List,
    Run { selector: String },
}

#[derive(Subcommand)]
pub enum DatabaseCommand {
    Migrate {
        #[arg(long)]
        database_url: Option<String>,
    },
}

#[derive(clap::ValueEnum, Clone)]
pub enum Language {
    Node,
    Rust,
    Python,
}

impl Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Node => write!(f, "node"),
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_defaults_without_schedules() {
        let cli = Cli::try_parse_from(["ryvus", "start"]).unwrap();

        assert!(matches!(cli.command, Command::Start { schedules: false }));
    }

    #[test]
    fn parses_database_migrate_url() {
        let cli = Cli::try_parse_from([
            "ryvus",
            "database",
            "migrate",
            "--database-url",
            "postgres://localhost/ryvus",
        ])
        .expect("database migrate command should parse");

        assert!(matches!(
            cli.command,
            Command::Database {
                command: DatabaseCommand::Migrate {
                    database_url: Some(url)
                }
            } if url == "postgres://localhost/ryvus"
        ));
    }
}
