use clap::Parser;
use ryvus_cli::{
    commands::{discover, new, start},
    error::CliError,
    Cli, Command,
};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
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

        Command::Start => start::run(),

        Command::Validate => todo!(),
    }
}
