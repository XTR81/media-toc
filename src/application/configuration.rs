use directories::ProjectDirs;
use gettextrs::gettext;
use ron;

use std::fs::{create_dir_all, File};
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use super::{TLD, SLD};

const CONFIG_FILENAME: &str = "config.ron";

lazy_static! {
    pub static ref CONFIG: Arc<RwLock<Config>> = Arc::new(RwLock::new(Config::new()));
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct UI {
    pub width: i32,
    pub height: i32,
    pub is_chapters_list_hidden: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Media {
    pub is_gl_disable: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct GlobalConfig {
    pub ui: UI,
    pub media: Media,
}

pub struct Config {
    path: PathBuf,
    last: GlobalConfig,
    current: GlobalConfig,
}

impl Config {
    fn new() -> Config {
        let project_dirs = ProjectDirs::from(TLD, SLD, env!("CARGO_PKG_NAME"));
        let config_dir = project_dirs.config_dir();
        create_dir_all(&config_dir).unwrap();
        let path = config_dir.join(CONFIG_FILENAME).to_owned();

        let last = match File::open(&path) {
            Ok(config_file) => {
                let config: Result<GlobalConfig, ron::de::Error> = ron::de::from_reader(config_file);
                match config {
                    Ok(config) => {
                        debug!("read config: {:?}", config);
                        config
                    }
                    Err(err) => {
                        error!("{}",
                            &gettext("couldn't load configuration: {}")
                                .replacen("{}", &format!("{:?}", err), 1),
                        );
                        GlobalConfig::default()
                    }
                }
            }
            Err(_) => GlobalConfig::default(),
        };

        Config {
            path,
            current: last.clone(),
            last,
        }
    }

    pub fn save(&mut self) {
        if self.last == self.current {
            // unchanged => don't save
            return;
        }

        match File::create(&self.path) {
            Ok(mut config_file) => {
                match ron::ser::to_string_pretty(
                    &self.current,
                    ron::ser::PrettyConfig::default(),
                ) {
                    Ok(config_str) => match config_file.write_all(config_str.as_bytes()) {
                        Ok(()) => {
                            self.last = self.current.clone();
                            debug!("saved config: {:?}", self.current);
                        }
                        Err(err) => {
                            error!("{}",
                                &gettext("couldn't write configuration: {}")
                                    .replacen("{}", &format!("{:?}", err), 1),
                            );
                        }
                    }
                    Err(err) => {
                        error!("{}",
                            &gettext("couldn't serialize configuration: {}")
                                .replacen("{}", &format!("{:?}", err), 1),
                        );
                    }
                }
            }
            Err(err) => {
                error!("{}",
                    &gettext("couldn't create configuration file: {}")
                        .replacen("{}", &format!("{:?}", err), 1),
                );
            }
        }
    }
}

impl Deref for Config {
    type Target = GlobalConfig;

    fn deref(&self) -> &Self::Target {
        &self.current
    }
}

impl DerefMut for Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.current
    }
}