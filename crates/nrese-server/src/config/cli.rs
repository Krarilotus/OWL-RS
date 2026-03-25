use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Result, bail};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliConfig {
    pub config_path: Option<PathBuf>,
}

impl CliConfig {
    pub fn from_args<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut config = Self::default();
        let mut args = args.into_iter();
        let _binary = args.next();

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

            bail!("unsupported argument: {:?}", argument);
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::CliConfig;

    #[test]
    fn cli_parser_accepts_separate_config_argument() {
        let config = CliConfig::from_args([
            OsString::from("nrese-server"),
            OsString::from("--config"),
            OsString::from("/etc/nrese/config.toml"),
        ])
        .expect("cli config");

        assert_eq!(
            config.config_path,
            Some(PathBuf::from("/etc/nrese/config.toml"))
        );
    }

    #[test]
    fn cli_parser_accepts_inline_config_argument() {
        let config = CliConfig::from_args([
            OsString::from("nrese-server"),
            OsString::from("--config=/etc/nrese/config.toml"),
        ])
        .expect("cli config");

        assert_eq!(
            config.config_path,
            Some(PathBuf::from("/etc/nrese/config.toml"))
        );
    }
}
