//! Command-line argument parsing.
//!
//! The accepted grammar is deliberately small: an optional preset name, `-h`
//! or `--help`, and `--` to stop option parsing so a preset name may start with
//! a dash. There are no other options.

use std::ffi::OsString;

const USAGE: &str = "Usage: copilot-switch [OPTIONS] [PRESET]";

/// What the caller asked the program to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Invocation {
    /// Print the preset help, including the available preset names.
    Help,
    /// Generate the configuration for one preset.
    Switch(String),
}

/// A command-line usage error, reported with exit status 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageError {
    /// An argument cannot be represented as UTF-8 and therefore cannot name a
    /// preset read from a UTF-8 JSON document.
    NonUtf8Argument(OsString),
    /// An option other than `-h` or `--help` was supplied.
    UnknownOption(String),
    /// More than one positional argument was supplied.
    UnexpectedArguments(Vec<String>),
}

impl UsageError {
    /// The complete diagnostic for this error, including the usage line.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NonUtf8Argument(argument) => format!(
                "{USAGE}\n\nError: argument is not valid UTF-8: {}",
                argument.to_string_lossy()
            ),
            Self::UnknownOption(option) => {
                format!("{USAGE}\n\nError: No such option: {option}")
            }
            Self::UnexpectedArguments(arguments) => format!(
                "{USAGE}\n\nError: Got unexpected extra argument(s) ({})",
                arguments.join(", ")
            ),
        }
    }
}

/// Parses the arguments after the program name.
///
/// Unknown options and more than one positional argument take precedence over
/// help. The first unknown option wins; otherwise every positional after the
/// first is reported as unexpected.
pub(crate) fn parse<I>(arguments: I) -> Result<Invocation, UsageError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut help = false;
    let mut positional_only = false;
    let mut positionals = Vec::new();
    let mut unknown_option = None;

    for argument in arguments {
        let text = argument.to_str().map(str::to_owned);
        let Some(text) = text else {
            return Err(UsageError::NonUtf8Argument(argument));
        };

        if !positional_only {
            if text == "--" {
                positional_only = true;
                continue;
            }
            if text == "-h" || text == "--help" {
                help = true;
                continue;
            }
            if text.starts_with('-') && text != "-" {
                unknown_option.get_or_insert(text);
                continue;
            }
        }

        positionals.push(text);
    }

    if let Some(option) = unknown_option {
        return Err(UsageError::UnknownOption(option));
    }
    if positionals.len() > 1 {
        return Err(UsageError::UnexpectedArguments(
            positionals.into_iter().skip(1).collect(),
        ));
    }
    if help {
        return Ok(Invocation::Help);
    }

    Ok(match positionals.pop() {
        Some(name) => Invocation::Switch(name),
        None => Invocation::Help,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(arguments: &[&str]) -> Result<Invocation, UsageError> {
        parse(arguments.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_and_help_requests_show_help() {
        assert_eq!(parse_args(&[]), Ok(Invocation::Help));
        assert_eq!(parse_args(&["-h"]), Ok(Invocation::Help));
        assert_eq!(parse_args(&["--help"]), Ok(Invocation::Help));
        assert_eq!(parse_args(&["work", "--help"]), Ok(Invocation::Help));
    }

    #[test]
    fn a_single_positional_is_the_selected_preset() {
        assert_eq!(
            parse_args(&["work-auto"]),
            Ok(Invocation::Switch("work-auto".to_owned()))
        );
    }

    #[test]
    fn double_dash_allows_dash_prefixed_preset_names() {
        assert_eq!(
            parse_args(&["--", "--help"]),
            Ok(Invocation::Switch("--help".to_owned()))
        );
        assert_eq!(
            parse_args(&["--", "-h"]),
            Ok(Invocation::Switch("-h".to_owned()))
        );
        assert_eq!(parse_args(&["-"]), Ok(Invocation::Switch("-".to_owned())));
    }

    #[test]
    fn unknown_options_and_extra_arguments_are_usage_errors() {
        assert_eq!(
            parse_args(&["--bogus"]),
            Err(UsageError::UnknownOption("--bogus".to_owned()))
        );
        assert_eq!(
            parse_args(&["-x"]),
            Err(UsageError::UnknownOption("-x".to_owned()))
        );
        assert_eq!(
            parse_args(&["work", "extra"]),
            Err(UsageError::UnexpectedArguments(vec!["extra".to_owned()]))
        );
        assert_eq!(
            parse_args(&["--", "work", "extra"]),
            Err(UsageError::UnexpectedArguments(vec!["extra".to_owned()]))
        );
    }

    #[test]
    fn usage_errors_win_over_help() {
        assert_eq!(
            parse_args(&["--help", "--bogus"]),
            Err(UsageError::UnknownOption("--bogus".to_owned()))
        );
        assert_eq!(
            parse_args(&["--help", "work", "extra"]),
            Err(UsageError::UnexpectedArguments(vec!["extra".to_owned()]))
        );
    }

    #[test]
    fn the_first_unknown_option_is_reported() {
        assert_eq!(
            parse_args(&["--first", "--second"]),
            Err(UsageError::UnknownOption("--first".to_owned()))
        );
    }
}
