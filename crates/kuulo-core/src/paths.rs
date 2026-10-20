use std::path::PathBuf;

use directories::ProjectDirs;

fn dirs() -> ProjectDirs {
    ProjectDirs::from("", "", "kuulo").expect("a home directory")
}

pub fn config_file() -> PathBuf {
    dirs().config_dir().join("config.json")
}

/// Where projects moved from the config file live.
pub fn projects_dir() -> PathBuf {
    dirs().config_dir().join("projects")
}

pub fn db_file() -> PathBuf {
    dirs().data_dir().join("library.sqlite")
}

/// Where copies go unless the config or the project says otherwise.
pub fn copies_dir() -> PathBuf {
    dirs().data_dir().join("copies")
}

pub fn peaks_cache_dir() -> PathBuf {
    dirs().cache_dir().join("peaks")
}

/// Where an update is downloaded and unpacked before it is installed.
pub fn update_dir() -> PathBuf {
    dirs().cache_dir().join("update")
}

/// Written by the Linux archive's `install.sh`: the folder it put the binaries in.
#[cfg(target_os = "linux")]
pub fn install_marker() -> PathBuf {
    dirs().data_dir().join("installed")
}

/// Writes `bytes` beside `path` and renames them over it, so a reader never sees half a file:
/// the window and the CLI each reload what the other saves. The temporary name carries the
/// process, so two writers never share one.
pub fn write_whole(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    ensure_parent(path)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

pub fn ensure_parent(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    Ok(())
}
