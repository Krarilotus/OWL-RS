use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Result, bail};

/// Command line: `nrese-server [--config PATH]` serves;
/// `nrese-server load [--config PATH] [--replace] [--graph IRI] [--skip-errors] FILE...` bulk-loads files
/// into the configured store and exits (the server must not be running on the same data
/// directory; the engine's directory lock enforces that);
/// `nrese-server query [--config PATH] [--format F] (QUERY | --file PATH)` answers one query
/// from the configured store on standard output (the same lock);
/// `nrese-server convert INPUT OUTPUT` converts an RDF file into another format (by the
/// extensions), without a store;
/// `nrese-server backup DIR` writes an image backup of the configured store into `DIR`
/// (the same lock: for a running server, `POST /ops/api/admin/dataset/image`);
/// `nrese-server prune-archive REVISION` removes the archived WAL segments whose records all
/// come before `REVISION` (safe while the server runs);
/// `nrese-server restore DIR [--wal LOGDIR]... [--until-revision N]` restores an image
/// backup into the configured data directory, which must hold no store, and with `--wal`
/// replays the WAL segments of those directories after it (up to revision `N`).
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
    Query(QueryCommand),
    Convert(ConvertCommand),
    /// `backup DIR`: an image backup of the store into `DIR`.
    Backup(PathBuf),
    /// `restore DIR`: the image backup in `DIR` into the configured data directory.
    Restore(RestoreCommand),
    /// `prune-archive REVISION`: archived WAL segments before `REVISION` removed.
    PruneArchive(Option<u64>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreCommand {
    pub backup: PathBuf,
    /// Directories of WAL segments to replay after the image (an archive, a live `wal/`).
    pub wal: Vec<PathBuf>,
    pub until_revision: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryCommand {
    /// The query text, or the file holding it.
    pub query: Option<String>,
    pub file: Option<PathBuf>,
    /// A results format (`json`, `xml`, `csv`, `tsv`) or, for CONSTRUCT and DESCRIBE, an
    /// RDF format by its extension (`nt`, `ttl`, `nq`, `trig`, `rdf`, `jsonld`).
    pub format: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConvertCommand {
    pub input: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadCommand {
    pub files: Vec<PathBuf>,
    pub replace: bool,
    /// Target graph IRI for triple formats; `None` = the default graph.
    pub graph: Option<String>,
    /// Skip statements with syntax errors (logged and counted) instead of stopping.
    pub skip_errors: bool,
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
        } else if args.peek().is_some_and(|argument| argument == "query") {
            args.next();
            config.command = CliCommand::Query(QueryCommand::default());
        } else if args.peek().is_some_and(|argument| argument == "convert") {
            args.next();
            config.command = CliCommand::Convert(ConvertCommand::default());
        } else if args.peek().is_some_and(|argument| argument == "backup") {
            args.next();
            config.command = CliCommand::Backup(PathBuf::new());
        } else if args.peek().is_some_and(|argument| argument == "restore") {
            args.next();
            config.command = CliCommand::Restore(RestoreCommand::default());
        } else if args
            .peek()
            .is_some_and(|argument| argument == "prune-archive")
        {
            args.next();
            config.command = CliCommand::PruneArchive(None);
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

            let text = |argument: OsString, what: &str| {
                argument
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("non-UTF-8 {what}"))
            };
            match &mut config.command {
                CliCommand::Query(query) => {
                    if argument == "--format" {
                        let Some(format) = args.next() else {
                            bail!("missing value for --format");
                        };
                        query.format = Some(text(format, "format")?);
                    } else if argument == "--file" {
                        let Some(file) = args.next() else {
                            bail!("missing value for --file");
                        };
                        query.file = Some(PathBuf::from(file));
                    } else if query.query.is_none() && query.file.is_none() {
                        query.query = Some(text(argument, "query")?);
                    } else {
                        bail!("unsupported argument: {:?}", argument);
                    }
                    continue;
                }
                CliCommand::Backup(dir) => {
                    if argument.to_str().is_some_and(|raw| raw.starts_with("--"))
                        || !dir.as_os_str().is_empty()
                    {
                        bail!("unsupported argument: {:?}", argument);
                    }
                    *dir = PathBuf::from(argument);
                    continue;
                }
                CliCommand::PruneArchive(revision) => {
                    if revision.is_some() {
                        bail!("unsupported argument: {:?}", argument);
                    }
                    let text = text(argument, "revision")?;
                    *revision = Some(
                        text.parse()
                            .map_err(|_| anyhow::anyhow!("`prune-archive` takes a revision"))?,
                    );
                    continue;
                }
                CliCommand::Restore(restore) => {
                    if argument == "--wal" {
                        let Some(dir) = args.next() else {
                            bail!("missing value for --wal");
                        };
                        restore.wal.push(PathBuf::from(dir));
                    } else if argument == "--until-revision" {
                        let Some(revision) = args.next().and_then(|r| r.into_string().ok()) else {
                            bail!("missing value for --until-revision");
                        };
                        restore.until_revision = Some(
                            revision
                                .parse()
                                .map_err(|_| anyhow::anyhow!("--until-revision takes a number"))?,
                        );
                    } else if argument.to_str().is_some_and(|raw| raw.starts_with("--"))
                        || !restore.backup.as_os_str().is_empty()
                    {
                        bail!("unsupported argument: {:?}", argument);
                    } else {
                        restore.backup = PathBuf::from(argument);
                    }
                    continue;
                }
                CliCommand::Convert(convert) => {
                    if argument.to_str().is_some_and(|raw| raw.starts_with("--")) {
                        bail!("unsupported argument: {:?}", argument);
                    } else if convert.input.as_os_str().is_empty() {
                        convert.input = PathBuf::from(argument);
                    } else if convert.output.as_os_str().is_empty() {
                        convert.output = PathBuf::from(argument);
                    } else {
                        bail!("`convert` takes an input and an output file");
                    }
                    continue;
                }
                _ => {}
            }
            let CliCommand::Load(load) = &mut config.command else {
                bail!("unsupported argument: {:?}", argument);
            };
            if argument == "--replace" {
                load.replace = true;
            } else if argument == "--skip-errors" {
                load.skip_errors = true;
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
        if let CliCommand::Query(query) = &config.command
            && query.query.is_some() == query.file.is_some()
        {
            bail!("`query` needs a query or --file (one of them)");
        }
        if let CliCommand::Convert(convert) = &config.command
            && convert.output.as_os_str().is_empty()
        {
            bail!("`convert` takes an input and an output file");
        }
        if config.command == CliCommand::PruneArchive(None) {
            bail!("`prune-archive` takes a revision");
        }
        if let CliCommand::Backup(dir) | CliCommand::Restore(RestoreCommand { backup: dir, .. }) =
            &config.command
            && dir.as_os_str().is_empty()
        {
            bail!("`backup` and `restore` take a backup directory");
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{CliCommand, CliConfig, ConvertCommand, LoadCommand, QueryCommand, RestoreCommand};

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
                skip_errors: false,
            })
        );
    }

    #[test]
    fn query_and_convert_take_their_arguments() {
        let config = parse(&["query", "--format", "tsv", "SELECT * {}"]).expect("cli config");
        assert_eq!(
            config.command,
            CliCommand::Query(QueryCommand {
                query: Some("SELECT * {}".to_owned()),
                file: None,
                format: Some("tsv".to_owned()),
            })
        );
        let config = parse(&["query", "-c", "n.toml", "--file", "q.rq"]).expect("cli config");
        assert_eq!(config.config_path, Some(PathBuf::from("n.toml")));
        assert!(parse(&["query"]).is_err());
        assert!(parse(&["query", "--file", "q.rq", "SELECT * {}"]).is_err());
        let config = parse(&["convert", "a.ttl", "b.nt"]).expect("cli config");
        assert_eq!(
            config.command,
            CliCommand::Convert(ConvertCommand {
                input: PathBuf::from("a.ttl"),
                output: PathBuf::from("b.nt"),
            })
        );
        assert!(parse(&["convert", "a.ttl"]).is_err());
        assert!(parse(&["convert", "a.ttl", "b.nt", "c.nq"]).is_err());
        let config = parse(&["backup", "-c", "n.toml", "/backups/one"]).expect("cli config");
        assert_eq!(
            config.command,
            CliCommand::Backup(PathBuf::from("/backups/one"))
        );
        assert_eq!(config.config_path, Some(PathBuf::from("n.toml")));
        let config = parse(&["restore", "/backups/one"]).expect("cli config");
        assert_eq!(
            config.command,
            CliCommand::Restore(RestoreCommand {
                backup: PathBuf::from("/backups/one"),
                ..RestoreCommand::default()
            })
        );
        let config = parse(&[
            "restore",
            "/b",
            "--wal",
            "/a",
            "--wal",
            "/w",
            "--until-revision",
            "42",
        ])
        .expect("cli config");
        assert_eq!(
            config.command,
            CliCommand::Restore(RestoreCommand {
                backup: PathBuf::from("/b"),
                wal: vec![PathBuf::from("/a"), PathBuf::from("/w")],
                until_revision: Some(42),
            })
        );
        assert!(parse(&["restore", "/b", "--until-revision", "x"]).is_err());
        let config = parse(&["prune-archive", "42"]).expect("cli config");
        assert_eq!(config.command, CliCommand::PruneArchive(Some(42)));
        assert!(parse(&["prune-archive"]).is_err());
        assert!(parse(&["prune-archive", "x"]).is_err());
        assert!(parse(&["backup"]).is_err());
        assert!(parse(&["restore", "a", "b"]).is_err());
    }

    #[test]
    fn load_options_are_rejected_when_serving_and_files_are_required() {
        assert!(parse(&["--replace"]).is_err());
        assert!(parse(&["data.nt"]).is_err());
        assert!(parse(&["load"]).is_err());
        assert!(parse(&["load", "--bogus", "a.nt"]).is_err());
    }
}
