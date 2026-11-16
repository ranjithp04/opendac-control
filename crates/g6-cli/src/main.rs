//! `g6` — command-line control for the Creative Sound BlasterX G6.

mod action;
mod cli;
mod format;
mod logger;
mod run;

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use g6_core::events::Config;
use g6_core::session::{Session, SessionConfig};
use g6_core::store::Paths;

use crate::cli::Cli;
use crate::run::{CliError, Env};

fn main() -> ExitCode {
    let cli = Cli::parse();
    logger::init(cli.verbose);

    let action = match action::build(cli.command) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("g6: {e}");
            return ExitCode::from(2);
        }
    };

    let paths = Paths::system();
    let session_paths = paths.clone();
    let mut env = Env::new(
        Box::new(move || {
            Session::open_hid(
                Config::default(),
                SessionConfig {
                    paths: session_paths.clone(),
                    reapply_on_first_connect: false,
                },
            )
        }),
        paths,
        Duration::from_secs(cli.connect_timeout),
    );

    match run::run(action, &mut env, &mut std::io::stdout().lock()) {
        Ok(()) | Err(CliError::BrokenPipe) => ExitCode::SUCCESS,
        Err(CliError::Usage(m)) => {
            eprintln!("g6: {m}");
            ExitCode::from(2)
        }
        Err(CliError::Failure(m)) => {
            eprintln!("g6: {m}");
            ExitCode::from(1)
        }
    }
}
