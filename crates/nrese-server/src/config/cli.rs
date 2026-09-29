use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Result, bail};

/// Command line: `nrese-server [--config PATH]` serves;
/// `nrese-server load [--config PATH] [--replace] [--graph IRI] FILE...` bulk-loads files
/// into the configured store and exits (the server must not be running on the same data
/// directory; the engine's directory lock enforces that).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliConfig {
    pub config_path: Option<PathBuf>,
    pub command: CliCommand,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CliCommand {
    #[default]
    Serve,
    Load(LoadCommand),
    /// `check-config`: load and validate the configuration, print the effective settings
    /// (secrets redacted) and exit.
    CheckConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadCommand {
    pub files: Vec<PathBuf>,
    pub replace: bool,
    /// Target graph IRI for triple formats; `None` = the default graph.
    pub graph: Option<String>,
}

impl CliConfig {
    pub fn from_args<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut config = Self::default();
        let mut args = args.into_iter().skip(1).peekable();
        if args.peek().is_some_and(|argument| argument == "load") {
            args.next();
            config.command = CliCommand::Load(LoadCommand::default());
        } else if args
            .peek()
            .is_some_and(|argument| argument == "check-config")
        {
            args.next();
            config.command = CliCommand::CheckConfig;
        }

        while let Some(argument) = args.next() {
            if argument == "--config" || argument == "-c" {
                let Some(path) = args.next() else {
                    bail!("missing value for {argument:?}");
                };
                config.config_path = Some(PathBuf::from(path));
                continue;
            }

            if let Some(value) = argument
                .to_str()
                .and_then(|raw| raw.strip_prefix("--config="))
            {
                config.config_path = Some(PathBuf::from(value));
                continue;
            }

            let CliCommand::Load(load) = &mut config.command else {
                bail!("unsupported argument: {:?}", argument);
            };
            if argument == "--replace" {
                load.replace = true;
            } else if argument == "--graph" {
                let Some(graph) = args.next().and_then(|graph| graph.into_string().ok()) else {
                    bail!("missing or non-UTF-8 value for --graph");
                };
                load.graph = Some(graph);
            } else if argument.to_str().is_some_and(|raw| raw.starts_with("--")) {
                bail!("unsupported argument: {:?}", argument);
            } else {
                load.files.push(PathBuf::from(argument));
            }
        }

        if let CliCommand::Load(load) = &config.command
            && load.files.is_empty()
        {
            bail!("`load` needs at least one RDF file");
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{CliCommand, CliConfig, LoadCommand};

    fn parse(args: &[&str]) -> anyhow::Result<CliConfig> {
        CliConfig::from_args(
            std::iter::once("nrese-server")
                .chain(args.iter().copied())
                .map(OsString::from),
        )
    }

    #[test]
    fn check_config_takes_a_config_path() {
        let config = parse(&["check-config", "--config", "a.toml"]).expect("cli config");
        assert_eq!(config.command, CliCommand::CheckConfig);
        assert_eq!(config.config_path, Some(PathBuf::from("a.toml")));
        assert!(parse(&["check-config", "--replace"]).is_err());
    }

    #[test]
    fn cli_parser_accepts_separate_config_argument() {
        let config = parse(&["--config", "/etc/nrese/config.toml"]).expect("cli config");

        assert_eq!(
            config.config_path,
            Some(PathBuf::from("/etc/nrese/config.toml"))
        );
        assert_eq!(config.command, CliCommand::Serve);
    }

    #[test]
    fn cli_parser_accepts_inline_config_argument() {
        let config = parse(&["--config=/etc/nrese/config.toml"]).expect("cli config");

        assert_eq!(
            config.config_path,
            Some(PathBuf::from("/etc/nrese/config.toml"))
        );
    }

    #[test]
    fn load_command_takes_files_and_options() {
        let config = parse(&[
            "load",
            "-c",
            "nrese.toml",
            "--replace",
            "--graph",
            "http://example.com/g",
            "a.nt",
            "b.ttl",
        ])
        .expect("cli config");
        assert_eq!(config.config_path, Some(PathBuf::from("nrese.toml")));
        assert_eq!(
            config.command,
            CliCommand::Load(LoadCommand {
                files: vec![PathBuf::from("a.nt"), PathBuf::from("b.ttl")],
                replace: true,
                graph: Some("http://example.com/g".to_owned()),
            })
        );
    }

    #[test]
    fn load_options_are_rejected_when_serving_and_files_are_required() {
        assert!(parse(&["--replace"]).is_err());
        assert!(parse(&["data.nt"]).is_err());
        assert!(parse(&["load"]).is_err());
        assert!(parse(&["load", "--bogus", "a.nt"]).is_err());
    }
}
