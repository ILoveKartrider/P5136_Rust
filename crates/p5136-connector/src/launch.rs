use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

use thiserror::Error;
use tokio::process::{Child, Command};

const GAME_EXECUTABLE: &str = "KartRider.exe";
const LAUNCH_ARGUMENT: &str = "-profile:launcher";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub game_directory: PathBuf,
}

impl LaunchRequest {
    #[must_use]
    pub fn new(game_directory: impl Into<PathBuf>) -> Self {
        Self {
            game_directory: game_directory.into(),
        }
    }

    #[must_use]
    pub fn executable(&self) -> PathBuf {
        self.game_directory.join(GAME_EXECUTABLE)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    Auto,
    Native,
    Wine {
        binary: PathBuf,
        prefix: Option<PathBuf>,
    },
    CrossOver {
        wine_binary: PathBuf,
        bottle: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub current_directory: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("CrossOver bottle name cannot be empty")]
    EmptyBottle,

    #[error("game executable does not exist: {0}")]
    MissingExecutable(PathBuf),

    #[error("failed to launch game")]
    Spawn(#[source] io::Error),
}

impl Runner {
    pub fn build(&self, request: &LaunchRequest) -> Result<LaunchSpec, LaunchError> {
        let resolved = match self {
            Self::Auto if cfg!(windows) => Self::Native,
            Self::Auto => Self::Wine {
                binary: PathBuf::from("wine"),
                prefix: None,
            },
            other => other.clone(),
        };
        let executable = request.executable();
        let current_directory = request.game_directory.clone();

        match resolved {
            Self::Native => Ok(LaunchSpec {
                program: executable,
                arguments: vec![OsString::from(LAUNCH_ARGUMENT)],
                current_directory,
                environment: Vec::new(),
            }),
            Self::Wine { binary, prefix } => {
                let mut environment = Vec::new();
                if let Some(prefix) = prefix {
                    environment.push((OsString::from("WINEPREFIX"), prefix.into_os_string()));
                }
                Ok(LaunchSpec {
                    program: binary,
                    arguments: vec![executable.into_os_string(), OsString::from(LAUNCH_ARGUMENT)],
                    current_directory,
                    environment,
                })
            }
            Self::CrossOver {
                wine_binary,
                bottle,
            } => {
                if bottle.trim().is_empty() {
                    return Err(LaunchError::EmptyBottle);
                }
                Ok(LaunchSpec {
                    program: wine_binary,
                    arguments: vec![
                        OsString::from("--bottle"),
                        OsString::from(bottle),
                        OsString::from("--cx-app"),
                        executable.into_os_string(),
                        OsString::from(LAUNCH_ARGUMENT),
                    ],
                    current_directory,
                    environment: Vec::new(),
                })
            }
            Self::Auto => unreachable!("the automatic runner is resolved before dispatch"),
        }
    }
}

impl LaunchSpec {
    pub fn validate(&self, game_executable: &Path) -> Result<(), LaunchError> {
        if game_executable.is_file() {
            Ok(())
        } else {
            Err(LaunchError::MissingExecutable(game_executable.to_owned()))
        }
    }

    pub fn spawn(&self, game_executable: &Path) -> Result<Child, LaunchError> {
        self.validate(game_executable)?;
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .current_dir(&self.current_directory);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.spawn().map_err(LaunchError::Spawn)
    }

    #[must_use]
    pub fn display(&self) -> String {
        std::iter::once(self.program.as_os_str())
            .chain(self.arguments.iter().map(OsString::as_os_str))
            .map(quote_for_display)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn quote_for_display(value: &OsStr) -> String {
    let text = value.to_string_lossy();
    if text.contains(char::is_whitespace) || text.contains('"') {
        format!("\"{}\"", text.replace('"', "\\\""))
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
    };

    use super::{LaunchRequest, Runner};

    #[test]
    fn wine_command_has_explicit_executable_cwd_and_prefix() {
        let request = LaunchRequest::new(Path::new("/games/Kart Rider"));
        let spec = Runner::Wine {
            binary: PathBuf::from("/usr/local/bin/wine64"),
            prefix: Some(PathBuf::from("/bottles/p5136")),
        }
        .build(&request)
        .unwrap();

        assert_eq!(spec.program, Path::new("/usr/local/bin/wine64"));
        assert_eq!(
            spec.arguments,
            [
                request.executable().into_os_string(),
                OsString::from("-profile:launcher"),
            ]
        );
        assert_eq!(spec.current_directory, Path::new("/games/Kart Rider"));
        assert_eq!(
            spec.environment,
            [(
                OsString::from("WINEPREFIX"),
                OsString::from("/bottles/p5136")
            )]
        );
    }

    #[test]
    fn crossover_command_keeps_the_bottle_explicit() {
        let request = LaunchRequest::new("/Games/KartRider");
        let spec = Runner::CrossOver {
            wine_binary: PathBuf::from(
                "/Applications/CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine",
            ),
            bottle: "KartRider-P5136".to_owned(),
        }
        .build(&request)
        .unwrap();

        assert_eq!(
            spec.arguments,
            [
                OsString::from("--bottle"),
                OsString::from("KartRider-P5136"),
                OsString::from("--cx-app"),
                request.executable().into_os_string(),
                OsString::from("-profile:launcher"),
            ]
        );
    }
}
