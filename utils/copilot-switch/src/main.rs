//! Copilot CLI configuration preset switcher.
//!
//! Generates `settings.json` and `mcp-config.json` in the repository root by
//! merging the selected preset from `config.presets.jsonc` into the shared
//! common configuration files.

mod agents;
mod cli;
mod config;
mod merge;
mod presets;
mod switch;

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let invocation = match cli::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("{}", error.message());
            return ExitCode::from(2);
        }
    };

    let config_dir = match config::config_dir() {
        Ok(directory) => directory,
        Err(error) => return fail(&error),
    };

    let preset = match invocation {
        cli::Invocation::Help => None,
        cli::Invocation::Switch(name) => Some(name),
    };

    match switch::run(&config_dir, preset.as_deref()) {
        Ok(output) => {
            let mut stdout = std::io::stdout().lock();
            if let Err(error) = stdout.write_all(output.as_bytes()) {
                eprintln!("Error: failed to write output: {error}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error),
    }
}

fn fail(error: &config::ConfigError) -> ExitCode {
    eprintln!("Error: {error}");
    ExitCode::FAILURE
}
