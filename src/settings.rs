use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub close_to_tray: bool,
    pub notifications_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            notifications_enabled: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        serde_json::from_reader(BufReader::new(File::open(path)?))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let temp = path.with_file_name("settings.json.tmp");
        let file = File::create(&temp)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        std::fs::rename(temp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_tray_and_notifications_enabled() {
        assert!(Settings::default().close_to_tray);
        assert!(Settings::default().notifications_enabled);
    }

    #[test]
    fn saves_and_loads_settings_atomically() {
        let dir = temp_dir("settings");
        let path = dir.join("settings.json");
        let settings = Settings {
            close_to_tray: false,
            notifications_enabled: false,
        };

        settings.save(&path).unwrap();
        Settings::default().save(&path).unwrap();

        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        assert!(!path.with_file_name("settings.json.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_{name}_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
