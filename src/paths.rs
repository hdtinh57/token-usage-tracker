use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

const DEFAULT_PRICING_JSON: &str = include_str!("../pricing_default.json");

pub struct Paths {
    pub pricing: PathBuf,
    pub settings: PathBuf,
    pub history: PathBuf,
}

impl Paths {
    pub fn new() -> io::Result<Self> {
        Self::from_app_data(Self::app_data_root(
            std::env::var_os("LOCALAPPDATA"),
            std::env::var_os("APPDATA"),
        )?)
    }

    pub fn app_data_root(
        local_app_data: Option<OsString>,
        app_data: Option<OsString>,
    ) -> io::Result<PathBuf> {
        local_app_data
            .or(app_data)
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "LOCALAPPDATA and APPDATA are not set",
                )
            })
    }

    pub fn from_app_data(app_data: PathBuf) -> io::Result<Self> {
        let root = app_data.join("TokenTracker");
        std::fs::create_dir_all(&root)?;
        let pricing = root.join("pricing.json");
        if !pricing.exists() {
            std::fs::write(&pricing, DEFAULT_PRICING_JSON)?;
        }
        Ok(Self {
            settings: root.join("settings.json"),
            history: root.join("history.json"),
            pricing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_app_data_files_under_token_tracker() {
        let root = std::env::temp_dir().join(format!(
            "tt_paths_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths::from_app_data(root.clone()).unwrap();

        let app_root = root.join("TokenTracker");
        assert_eq!(paths.pricing, app_root.join("pricing.json"));
        assert_eq!(paths.settings, app_root.join("settings.json"));
        assert_eq!(paths.history, app_root.join("history.json"));
        assert!(paths.pricing.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prefers_local_app_data_and_falls_back_to_app_data() {
        assert_eq!(
            Paths::app_data_root(Some("local".into()), Some("roaming".into())).unwrap(),
            std::path::PathBuf::from("local")
        );
        assert_eq!(
            Paths::app_data_root(None, Some("roaming".into())).unwrap(),
            std::path::PathBuf::from("roaming")
        );
    }
}
