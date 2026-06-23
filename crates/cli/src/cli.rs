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
    Start,
    Validate,
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
