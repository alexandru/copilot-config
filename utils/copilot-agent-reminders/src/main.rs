//! The copilot-agent-reminders command hook for Copilot.
//!
//! Reads one Copilot hook payload from stdin, evolves the persistent reminder
//! state, and writes the documented command-hook response to stdout.

mod config;
mod hook;
mod machine;
mod storage;
mod time;

use std::process::ExitCode;

fn main() -> ExitCode {
    hook::run(std::env::args_os().skip(1))
}
