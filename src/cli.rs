//! `xfeed serve` and `xfeed update`.

use std::path::PathBuf;

use crate::store::UpdateTarget;

pub enum Command {
    Serve {
        config: PathBuf,
    },
    Update {
        config: PathBuf,
        target: UpdateTarget,
    },
    Prune {
        config: PathBuf,
    },
}

pub fn usage() -> &'static str {
    "\
usage: xfeed [serve] [--config path] [config.yml]
       xfeed update [--config path] [--tab name | --account handle] [config.yml]
       xfeed prune [--config path] [config.yml]"
}

pub fn parse_args<I, T>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = T>,
    T: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Err(usage().to_string());
    }
    let (mode, rest) = match args.first().map(String::as_str) {
        Some("update") => (Mode::Update, &args[1..]),
        Some("prune") => (Mode::Prune, &args[1..]),
        Some("serve") => (Mode::Serve, &args[1..]),
        _ => (Mode::Serve, args.as_slice()),
    };

    let mut config: Option<PathBuf> = None;
    let mut tab = None;
    let mut account = None;
    let mut index = 0;
    while index < rest.len() {
        let arg = rest[index].as_str();
        match arg {
            "--config" => {
                index += 1;
                let path = rest
                    .get(index)
                    .ok_or_else(|| format!("--config needs a path\n{}", usage()))?;
                config = Some(PathBuf::from(path));
            }
            "--tab" => {
                index += 1;
                let name = rest
                    .get(index)
                    .ok_or_else(|| format!("--tab needs a name\n{}", usage()))?;
                if tab.is_some() {
                    return Err(format!("--tab given twice\n{}", usage()));
                }
                tab = Some(name.clone());
            }
            "--account" => {
                index += 1;
                let name = rest
                    .get(index)
                    .ok_or_else(|| format!("--account needs a handle\n{}", usage()))?;
                if account.is_some() {
                    return Err(format!("--account given twice\n{}", usage()));
                }
                account = Some(name.clone());
            }
            "--" => {
                return Err(format!("unexpected argument\n{}", usage()));
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n{}", usage()));
            }
            other => {
                if config.is_some() {
                    return Err(format!("unexpected argument {other}\n{}", usage()));
                }
                config = Some(PathBuf::from(other));
            }
        }
        index += 1;
    }

    if mode != Mode::Update && (tab.is_some() || account.is_some()) {
        return Err(format!(
            "--tab and --account are only valid with update\n{}",
            usage()
        ));
    }
    if tab.is_some() && account.is_some() {
        return Err(format!("pass only one of --tab or --account\n{}", usage()));
    }
    let config = config.unwrap_or_else(|| PathBuf::from("config.yml"));
    match mode {
        Mode::Serve => Ok(Command::Serve { config }),
        Mode::Prune => Ok(Command::Prune { config }),
        Mode::Update => {
            let target = match (tab, account) {
                (Some(name), None) => UpdateTarget::Tab(name),
                (None, Some(name)) => UpdateTarget::Account(name),
                (None, None) => UpdateTarget::All,
                (Some(_), Some(_)) => unreachable!(),
            };
            Ok(Command::Update { config, target })
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Serve,
    Update,
    Prune,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serve_and_update() {
        match parse_args(Vec::<String>::new()).unwrap() {
            Command::Serve { config } => assert_eq!(config, PathBuf::from("config.yml")),
            _ => panic!("expected serve"),
        }
        match parse_args(["serve", "app.yml"]).unwrap() {
            Command::Serve { config } => assert_eq!(config, PathBuf::from("app.yml")),
            _ => panic!("expected serve"),
        }
        match parse_args(["--config", "app.yml"]).unwrap() {
            Command::Serve { config } => assert_eq!(config, PathBuf::from("app.yml")),
            _ => panic!("expected serve"),
        }
        match parse_args(["prune", "--config", "c.yml"]).unwrap() {
            Command::Prune { config } => assert_eq!(config, PathBuf::from("c.yml")),
            _ => panic!("expected prune"),
        }
        match parse_args(["update"]).unwrap() {
            Command::Update {
                target: UpdateTarget::All,
                config,
            } => assert_eq!(config, PathBuf::from("config.yml")),
            _ => panic!("expected update all"),
        }
        match parse_args(["update", "--tab", "News", "other.yml"]).unwrap() {
            Command::Update {
                target: UpdateTarget::Tab(name),
                config,
            } => {
                assert_eq!(name, "News");
                assert_eq!(config, PathBuf::from("other.yml"));
            }
            _ => panic!("expected tab"),
        }
        match parse_args(["update", "--config", "c.yml", "--account", "@alice"]).unwrap() {
            Command::Update {
                target: UpdateTarget::Account(name),
                config,
            } => {
                assert_eq!(name, "@alice");
                assert_eq!(config, PathBuf::from("c.yml"));
            }
            _ => panic!("expected account"),
        }
        assert!(parse_args(["update", "--tab", "News", "--account", "alice"]).is_err());
        assert!(parse_args(["serve", "--tab", "News"]).is_err());
        assert!(parse_args(["prune", "--account", "alice"]).is_err());
        assert!(parse_args(["update", "--nope"]).is_err());
    }
}
