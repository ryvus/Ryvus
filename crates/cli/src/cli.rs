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
        #[arg(long)]
        long_lived: bool,
    },
    Validate,
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    List,
    Run { selector: String },
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
    fn start_defaults_to_per_invocation() {
        let cli = Cli::try_parse_from(["ryvus", "start"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Start {
                schedules: false,
                long_lived: false
            }
        ));
    }

    #[test]
    fn start_accepts_long_lived_with_schedules() {
        let cli = Cli::try_parse_from(["ryvus", "start", "--schedules", "--long-lived"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Start {
                schedules: true,
                long_lived: true
            }
        ));
    }
}
