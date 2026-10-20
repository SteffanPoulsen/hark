//! Whether a newer kuulo is out, and installing it over a build that kuulo's own installer put
//! in place. GitHub is asked only when the user asks: for the latest release, and for its
//! download when the user chooses to install it.

use anyhow::{anyhow, bail, Context, Result};
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::OnceLock;

pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The minisign public keys an update must be signed with, the files `sign-release.sh` checks
/// against: the release key, and an offline backup that takes over if it is ever lost.
const KEYS: &[&str] = &[include_str!("../../../packaging/kuulo.pub"), include_str!("../../../packaging/kuulo-backup.pub")];

/// A packager's word on how their build is updated. Set at build time, it turns the updater
/// off, since the package manager owns the files.
pub const PACKAGED: Option<&str> = option_env!("KUULO_UPDATE_EXPLANATION");

#[derive(Clone, Debug, Serialize)]
pub struct Release {
    pub version: String,
    /// The release's page, with its notes and downloads.
    pub url: String,
    pub newer: bool,
    #[serde(skip)]
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Clone, Copy, Debug)]
pub enum Progress {
    Downloading { done: u64, total: Option<u64> },
    Installing,
}

pub fn releases_url() -> String {
    format!("{REPOSITORY}/releases")
}

fn request(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    ureq::get(url).header("User-Agent", &format!("kuulo/{}", crate::VERSION))
}

/// The latest release on GitHub, or the one `KUULO_UPDATE_FEED` names: a URL serving a release
/// in GitHub's format, for trying an update before a release (`packaging/update-feed.sh`). A
/// feed decides only what is offered; what installs must still carry kuulo's signature under
/// its own name, so a feed can point anywhere.
pub fn latest() -> Result<Release> {
    let url = match std::env::var("KUULO_UPDATE_FEED") {
        Ok(feed) => feed,
        Err(_) => {
            let repo = REPOSITORY.strip_prefix("https://github.com/").ok_or_else(|| anyhow!("{REPOSITORY} is not a GitHub repository"))?;
            format!("https://api.github.com/repos/{repo}/releases/latest")
        }
    };
    let r: GithubRelease = request(&url)
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_json()?;
    let version = r.tag_name.trim_start_matches('v').to_string();
    let newer = is_newer(&version, crate::VERSION);
    Ok(Release { version, url: r.html_url, newer, assets: r.assets })
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

pub(crate) fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('-').next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(candidate) > parts(current)
}

/// Where kuulo's own installer put this build: the folder holding the binaries on Linux and
/// Windows, the app bundle on macOS. Only such a build installs updates itself; one from a
/// package manager, a source build or the zip is updated the way it came.
pub fn own_install() -> Option<&'static Path> {
    // Asked once: after an update replaces the binary, Linux reports the running one's path
    // as deleted.
    static OWN: OnceLock<Option<PathBuf>> = OnceLock::new();
    OWN.get_or_init(|| {
        if PACKAGED.is_some() {
            return None;
        }
        find_own_install(&std::env::current_exe().ok()?.canonicalize().ok()?)
    })
    .as_deref()
}

#[cfg(target_os = "linux")]
fn find_own_install(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let marked = std::fs::read_to_string(crate::paths::install_marker()).ok()?;
    (Path::new(marked.trim()).canonicalize().ok()? == dir).then(|| dir.to_path_buf())
}

#[cfg(windows)]
fn find_own_install(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    dir.join("unins000.exe").is_file().then(|| dir.to_path_buf())
}

#[cfg(target_os = "macos")]
fn find_own_install(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension()? == "app").then(|| bundle.to_path_buf())
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn find_own_install(_: &Path) -> Option<PathBuf> {
    None
}

/// The file of a release that this platform installs from.
fn asset_name(version: &str) -> Option<String> {
    let platform = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64.tar.gz"
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        "windows-x86_64-setup.exe"
    } else if cfg!(target_os = "macos") {
        "macos-universal.tar.gz"
    } else {
        return None;
    };
    Some(format!("kuulo-{version}-{platform}"))
}

/// Whether [`install`] returns before the new version is in place, which is so where the
/// installer has to wait for this process to exit.
pub const INSTALLS_AFTER_EXIT: bool = cfg!(windows);

/// Downloads this platform's file of `release`, checks its signature, and installs it over
/// this build; nothing is installed unless the signature holds. With `restart`, the new
/// kuulo window starts once this process has exited. The caller exits soon after either way.
pub fn install(release: &Release, restart: bool, progress: &mut dyn FnMut(Progress)) -> Result<()> {
    let own = own_install().ok_or_else(|| match PACKAGED {
        Some(explanation) => anyhow!("{explanation}"),
        None => anyhow!("kuulo's own installer did not put this build here, so it cannot update itself; kuulo {} is at {}", release.version, release.url),
    })?;
    // An offer can wait in an open window while another kuulo installs something newer.
    let now = latest()?;
    if now.version != release.version {
        bail!("kuulo {} is the latest release now; check for updates again", now.version);
    }
    let name = asset_name(&release.version).ok_or_else(|| anyhow!("kuulo publishes no build for this platform"))?;
    let asset = release.asset(&name).ok_or_else(|| anyhow!("kuulo {} has no {name}", release.version))?;
    let signature = release.asset(&format!("{name}.minisig")).ok_or_else(|| anyhow!("kuulo {} is not signed, so it is not installed", release.version))?;
    let signature = request(&signature.browser_download_url).call()?.body_mut().read_to_string()?;
    let dir = staging_dir()?;
    let file = dir.join(&name);
    download(&asset.browser_download_url, &file, progress)?;
    verify(std::fs::File::open(&file)?, &signature, &name, KEYS).with_context(|| format!("{name} is not the file kuulo's release key signed"))?;
    progress(Progress::Installing);
    apply(&file, own, restart)
}

/// A folder of this process's own for the download, so two installs at once never read each
/// other's files. A folder left by an earlier install, from a crash or a Windows setup that
/// was still running from it, goes once it is an hour old.
fn staging_dir() -> Result<PathBuf> {
    let root = crate::paths::update_dir();
    let dir = root.join(std::process::id().to_string());
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() > 3600));
        if entry.path() != dir && old {
            let path = entry.path();
            let _ = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
    Ok(dir)
}

fn download(url: &str, to: &Path, progress: &mut dyn FnMut(Progress)) -> Result<()> {
    let mut response = request(url).call()?;
    let total = response.body().content_length();
    let mut body = response.body_mut().as_reader();
    let mut out = std::fs::File::create(to)?;
    let mut buf = vec![0; 1 << 16];
    let mut done = 0;
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress(Progress::Downloading { done, total });
    }
}

/// Checks the minisign `signature` over `data` against `keys`, and that the file it was made
/// for is `asset`. The name carries the version and the platform, so neither an older
/// release's file nor another platform's passes for this one.
fn verify(mut data: impl Read, signature: &str, asset: &str, keys: &[&str]) -> Result<()> {
    let signature = Signature::decode(signature)?;
    let keys = keys.iter().map(|k| PublicKey::decode(k)).collect::<Result<Vec<_>, _>>()?;
    let mut verifier = keys.iter().find_map(|k| k.verify_stream(&signature).ok()).ok_or_else(|| anyhow!("it is signed with a key kuulo does not know"))?;
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = data.read(&mut buf)?;
        if n == 0 {
            break;
        }
        verifier.update(&buf[..n]);
    }
    verifier.finalize()?;
    // minisign's own trusted comment names the file: `timestamp:…\tfile:<name>\thashed`.
    if !signature.trusted_comment().split('\t').any(|field| field.strip_prefix("file:") == Some(asset)) {
        bail!("its signature was made for another file ({})", signature.trusted_comment());
    }
    Ok(())
}

/// Unpacks the release archive and runs its own `install.sh`, which puts the binaries, the
/// desktop entry and the icons where the last one did.
#[cfg(target_os = "linux")]
fn apply(archive: &Path, own: &Path, restart: bool) -> Result<()> {
    let dir = archive.parent().expect("the archive is in the update folder");
    run(Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(dir))?;
    let unpacked = archive.to_string_lossy().trim_end_matches(".tar.gz").to_string();
    run(Command::new("sh").arg(Path::new(&unpacked).join("install.sh")))?;
    let _ = std::fs::remove_dir_all(dir);
    if restart {
        start_after_exit(own)?;
    }
    Ok(())
}

/// Starts the new setup silently. It closes a kuulo still running through the Restart
/// Manager, so it finishes after this process has exited, and with `/LAUNCH=1` it starts
/// kuulo again. Its message boxes stay on, since by then no kuulo is left to report an
/// error. Running the setup, not swapping the files, keeps the uninstall entry's version
/// right.
#[cfg(windows)]
fn apply(setup: &Path, _own: &Path, restart: bool) -> Result<()> {
    let mut command = std::process::Command::new(setup);
    command.args(["/VERYSILENT", "/NORESTART"]);
    if restart {
        command.arg("/LAUNCH=1");
    }
    command.spawn().with_context(|| format!("{}", setup.display()))?;
    Ok(())
}

/// Unpacks the new app beside the installed one, on its volume, and exchanges the two in one
/// step, so there is never a moment without a kuulo there. A running kuulo keeps the files
/// it has open, so the bundle is swapped whole and never edited in place, which would break
/// the running code's signature.
#[cfg(target_os = "macos")]
fn apply(archive: &Path, own: &Path, restart: bool) -> Result<()> {
    let staging = own.with_file_name(format!(".kuulo-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging).with_context(|| format!("{}", staging.display()))?;
    let swapped = (|| {
        run(Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(&staging))?;
        let new = staging.join("kuulo.app");
        if !new.is_dir() {
            bail!("{} holds no kuulo.app", archive.display());
        }
        swap(&new, own)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir_all(archive.parent().expect("the archive is in the update folder"));
    swapped?;
    if restart {
        start_after_exit(own)?;
    }
    Ok(())
}

/// Exchanges what two paths name in one step.
#[cfg(target_os = "macos")]
fn swap(a: &Path, b: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes());
    let (ca, cb) = (c(a)?, c(b)?);
    // SAFETY: both are NUL-terminated paths that outlive the call.
    if unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("swapping the new kuulo into {}", b.display()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn apply(_: &Path, _: &Path, _: bool) -> Result<()> {
    bail!("kuulo cannot install its updates on this platform yet")
}

/// Starts the installed kuulo once this process has exited, so the two never hold the audio
/// device or the library at once.
#[cfg(unix)]
fn start_after_exit(own: &Path) -> Result<()> {
    let start = if cfg!(target_os = "macos") { "exec open \"$0\"" } else { "exec \"$0/kuulo\" </dev/null" };
    // The waiter reads a pipe that only this process holds open. Exit closes it even while
    // the process waits to be reaped, and unlike a pid it cannot come to name another process.
    let mut waiter = Command::new("sh").arg("-c").arg(format!("cat >/dev/null; {start}")).arg(own).stdin(std::process::Stdio::piped()).spawn()?;
    std::mem::forget(waiter.stdin.take());
    Ok(())
}

#[cfg(unix)]
fn run(command: &mut Command) -> Result<()> {
    let out = command.output().with_context(|| format!("{command:?}"))?;
    if !out.status.success() {
        bail!("{command:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_newer, verify};

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.10.0", "0.9.3"));
        assert!(is_newer("0.4.0", "0.3.1"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.0", "0.3.1"));
    }

    const KEY: &str = "untrusted comment: test key\nRWS7Zmec8lgGYtpJATUilWIc7R646bBGeost1Ep03ah9VXjjMhLxctFr\n";
    const OTHER_KEY: &str = "untrusted comment: another test key\nRWR/J3RlKW3ZiT3zOXEj7MRIds7+xpiegFH4SHV8dTPTGEt0Ynl+QJj9\n";
    const ASSET: &str = "kuulo-9.9.9-linux-x86_64.tar.gz";
    const DATA: &[u8] = b"kuulo update test payload";
    const SIGNATURE: &str = "untrusted comment: signature from the test key
RUS7Zmec8lgGYhnXtL0J8zslCE4TVjttTwUEQRcBF2S6fUMf05skxWvmw+o/zm908p/iCBNQgdwjfcxYRJ/Xe+yVFbBXcg23Tgg=
trusted comment: timestamp:1792483200\tfile:kuulo-9.9.9-linux-x86_64.tar.gz\thashed
iaWETNH/2FJBKS7XBvtFc7M7UfdOv8kEbe1WKpnugXYSggANyzA4xBfy4soUZqpSbXzM/wZIazmgJCAdNjgcCw==
";

    #[test]
    fn only_the_signed_file_under_its_own_name_passes() {
        assert!(verify(DATA, SIGNATURE, ASSET, &[OTHER_KEY, KEY]).is_ok());
        assert!(verify(&b"kuulo update test payloaD"[..], SIGNATURE, ASSET, &[KEY]).is_err());
        assert!(verify(DATA, SIGNATURE, "kuulo-9.9.8-linux-x86_64.tar.gz", &[KEY]).is_err());
        assert!(verify(DATA, SIGNATURE, ASSET, &[OTHER_KEY]).is_err());
    }
}
