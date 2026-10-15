use std::path::PathBuf;

use directories::ProjectDirs;

fn dirs() -> ProjectDirs {
    ProjectDirs::from("", "", "hark").expect("a home directory")
}

pub fn config_file() -> PathBuf {
    dirs().config_dir().join("config.json")
}

pub fn db_file() -> PathBuf {
    dirs().data_dir().join("library.sqlite")
}

pub fn peaks_cache_dir() -> PathBuf {
    dirs().cache_dir().join("peaks")
}

pub fn ensure_parent(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    Ok(())
}
