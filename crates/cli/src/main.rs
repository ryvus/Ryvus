use clap::Parser;
use ryvus_cli::{
    commands::{database, discover, new, schedule, start, validate},
    error::CliError,
    Cli, Command, DatabaseCommand, ScheduleCommand,
};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();

    if let Err(err) = run(cli) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::New {
            project_name,
            language,
        } => new::run(project_name, language),

        Command::Discover => discover::run(),

        Command::Start { schedules } => start::run(schedules),

        Command::Validate => validate::run(),

        Command::Schedule { command } => match command {
            ScheduleCommand::List => schedule::list(),
            ScheduleCommand::Run { selector } => schedule::run(selector),
        },

        Command::Database { command } => match command {
            DatabaseCommand::Migrate { database_url } => database::migrate(database_url),
        },
    }
}
