use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use hark_core::audition::{Clip, Source};
use hark_core::config::Root;
use hark_core::export::{ExportRequest, Selection};
use hark_core::library::{ChannelClass, Query, Sort, SortKey};
use hark_core::{Config, Db};

#[derive(Args)]
struct SortArgs {
    /// Order: name, folder, length, channels, rate, bits, codec, loudness or size.
    #[arg(long, default_value = "name")]
    sort: SortKey,
    /// Reverse the order.
    #[arg(long)]
    desc: bool,
}

impl From<SortArgs> for Sort {
    fn from(a: SortArgs) -> Sort {
        Sort { key: a.sort, descending: a.desc }
    }
}

#[derive(Parser)]
#[command(name = "hark-cli", version, about = "hark without the window: library, marks, analysis, export and copy")]
struct Cli {
    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Act in this project file (or a known project's name) instead of the open one.
    #[arg(long, global = true)]
    project: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Project files: an output folder, a library scope and favorites of their own.
    Projects {
        #[command(subcommand)]
        cmd: Option<ProjectsCmd>,
    },
    /// Library folders of the open project, or of the default state.
    Roots {
        #[command(subcommand)]
        cmd: Option<RootsCmd>,
    },
    /// Walk the roots and refresh the library database.
    Scan {
        /// Root name or path; all roots when omitted.
        root: Option<String>,
    },
    /// Files directly inside a folder.
    Ls {
        dir: PathBuf,
        #[command(flatten)]
        sort: SortArgs,
    },
    /// Search the library by path words, tags, favorite, file type and channels.
    Search {
        words: Vec<String>,
        /// Files carrying any of the given tags; repeatable.
        #[arg(long)]
        tag: Vec<String>,
        #[arg(long)]
        fav: bool,
        /// File extension such as wav or flac; repeatable.
        #[arg(long = "type")]
        extension: Vec<String>,
        /// mono, stereo or multi; repeatable.
        #[arg(long)]
        channels: Vec<ChannelClass>,
        #[arg(long)]
        under: Option<PathBuf>,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[command(flatten)]
        sort: SortArgs,
    },
    /// Everything known about a file: probe, library record, and the origin it carries if hark exported it.
    Inspect { file: PathBuf },
    /// The spans of sound in a file, parted by silence: the takes a recording holds
    Takes { file: PathBuf },
    /// Measure loudness and peaks.
    Analyze {
        file: PathBuf,
        /// Store the result in the library.
        #[arg(long)]
        store: bool,
    },
    /// Toggle tags: `+sfx` adds, `-sfx` removes.
    Tag { file: PathBuf, changes: Vec<String> },
    /// Mark or unmark a favorite.
    Fav {
        file: PathBuf,
        #[arg(long)]
        off: bool,
    },
    /// Defined tags and how many files carry each; add, change, rename, reorder or remove them.
    Tags {
        #[command(subcommand)]
        cmd: Option<TagsCmd>,
    },
    /// Export presets.
    Presets {
        #[command(subcommand)]
        cmd: Option<PresetsCmd>,
    },
    /// The window's keys: each action's chord, and changing one.
    Keys {
        #[command(subcommand)]
        cmd: Option<KeysCmd>,
    },
    /// Export a file, or a selection of it, through a preset.
    Export(ExportArgs),
    /// The copy folder: what it holds, where it is, and clearing it.
    Copies {
        #[command(subcommand)]
        cmd: Option<CopiesCmd>,
    },
    /// Run an exported file's own recipe again, with changes.
    Reexport {
        files: Vec<PathBuf>,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        output_root: Option<PathBuf>,
        /// Write next to the original export instead of the template path.
        #[arg(long)]
        in_place: bool,
    },
    /// Show or change settings.
    Config {
        #[arg(long)]
        output_root: Option<PathBuf>,
        #[arg(long)]
        default_preset: Option<String>,
        /// Window zoom on top of the display scale.
        #[arg(long)]
        ui_zoom: Option<f32>,
        /// Sample rate the audition engine asks the device for; 0 takes the device default.
        #[arg(long)]
        audition_rate: Option<u32>,
        /// Files whose decoded audio is larger than this many megabytes stream from disk.
        #[arg(long)]
        stream_above_mb: Option<f64>,
        /// Print the config file path.
        #[arg(long)]
        path: bool,
    },
    /// Play a file through the audition engine and wait for it to finish. Files whose decoded
    /// audio is larger than the config's `stream_above_mb` stream from disk while they play.
    Play {
        file: PathBuf,
        #[arg(long = "in")]
        start: Option<f64>,
        #[arg(long = "out")]
        end: Option<f64>,
        /// Start inside the selection, with the fades where they would be from `--in`.
        #[arg(long)]
        from: Option<f64>,
        /// Repeat the selection; plays for `--for` seconds, or until interrupted.
        #[arg(long = "loop")]
        looped: bool,
        /// Stop after this many seconds of playback.
        #[arg(long = "for")]
        seconds: Option<f64>,
        /// After AT seconds of playback restart from TO seconds, as a waveform click would.
        #[arg(long, value_name = "AT:TO", value_parser = parse_jump)]
        jump: Option<(f64, f64)>,
        /// Stream when the decoded audio is larger than this many megabytes, overriding the
        /// config for this run.
        #[arg(long, value_name = "MB")]
        stream_above: Option<f64>,
    },
}

#[derive(Subcommand)]
enum RootsCmd {
    List,
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    Remove { name: String },
}

#[derive(Subcommand)]
enum ProjectsCmd {
    /// The known project files, the open one starred.
    List,
    /// Write a new, empty project file and open it.
    New {
        file: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Open a project file, or a known project by name.
    Open { project: String },
    /// Back to the default state: whole library, default output folder, default favorites.
    Close,
    /// Drop a project from the known list. The file and its favorites stay.
    Forget { project: String },
    /// Set the output folder of the open project, or the default one.
    Output { dir: PathBuf },
}

#[derive(Subcommand)]
enum CopiesCmd {
    /// The folder, its files newest first, and their total size.
    List,
    /// Remove every file in the folder. A DAW that references a copy in place loses it.
    Clear,
    /// Show or set the folder: the open project's own, or the default with no project open
    /// or with --default.
    Folder {
        dir: Option<PathBuf>,
        /// Set the default folder even while a project is open.
        #[arg(long)]
        default: bool,
        /// Back to the fallback: the default folder for a project, hark's own for the default.
        #[arg(long)]
        unset: bool,
    },
}

#[derive(Subcommand)]
enum TagsCmd {
    List,
    Add {
        name: String,
        #[arg(long)]
        color: Option<String>,
    },
    /// Change a tag's color; an empty value clears it.
    Set {
        name: String,
        #[arg(long)]
        color: Option<String>,
    },
    /// Rename a tag and rewrite the marks that carry it.
    Rename { old: String, new: String },
    /// Move a tag to a position (0-based); position decides the 1-9 hotkey.
    Move { name: String, position: usize },
    /// Remove a tag definition and the marks that carry it.
    Remove { name: String },
}

#[derive(Subcommand)]
enum KeysCmd {
    List,
    /// Bind an action to a chord such as `E`, `Ctrl+Shift+E` or `ArrowLeft`; key names are
    /// the window's.
    Set { action: String, chord: String },
    /// An action back to its default, or every action with none named.
    Reset { action: Option<String> },
}

#[derive(Subcommand)]
enum PresetsCmd {
    List,
    Show { name: String },
    /// A new preset with the default values, or a copy of another.
    Add {
        name: String,
        #[arg(long)]
        from: Option<String>,
    },
    /// Change a preset's values; only the given ones change.
    Set {
        name: String,
        #[command(flatten)]
        values: PresetValues,
    },
    /// Rename a preset; tags and the default follow it.
    Rename { old: String, new: String },
    /// Remove a preset; tags that selected it select none.
    Remove { name: String },
}

#[derive(Args)]
struct PresetValues {
    /// wav or flac.
    #[arg(long)]
    format: Option<hark_core::config::OutputFormat>,
    /// 16, 24 or 32 (WAV only).
    #[arg(long)]
    bits: Option<u16>,
    /// Output sample rate in Hz, or "source".
    #[arg(long)]
    rate: Option<String>,
    /// "source", or a downmix: average, sum, left, right.
    #[arg(long)]
    channels: Option<String>,
    /// none, peak or lufs.
    #[arg(long)]
    normalize: Option<String>,
    /// Peak target in dBFS or LUFS target (negative: --target=-16).
    #[arg(long, allow_hyphen_values = true)]
    target: Option<f64>,
    /// True-peak ceiling in dBTP for LUFS normalisation.
    #[arg(long, allow_hyphen_values = true)]
    ceiling: Option<f64>,
    #[arg(long)]
    dither: Option<bool>,
    /// linear, equal_power or logarithmic.
    #[arg(long)]
    fade: Option<hark_core::pcm::FadeShape>,
    /// Output path template under the output folder.
    #[arg(long)]
    template: Option<String>,
}

impl PresetValues {
    fn apply(&self, p: &mut hark_core::ExportPreset) -> Result<()> {
        use hark_core::config::Normalize;
        if let Some(f) = self.format {
            p.format = f;
        }
        if let Some(b) = self.bits {
            if !matches!(b, 16 | 24 | 32) {
                return Err(anyhow!("bits must be 16, 24 or 32"));
            }
            p.bits = b;
        }
        if let Some(r) = &self.rate {
            p.sample_rate = if r == "source" { None } else { Some(r.parse().map_err(|_| anyhow!("rate must be a number in Hz or source"))?) };
        }
        if let Some(c) = &self.channels {
            p.mono = if c == "source" { None } else { Some(c.parse().map_err(|e| anyhow!("{e}"))?) };
        }
        if let Some(n) = &self.normalize {
            p.normalize = match n.as_str() {
                "none" => Normalize::None,
                "peak" => Normalize::Peak { target_dbfs: -1.0 },
                "lufs" => Normalize::Lufs { target_lufs: -16.0, true_peak_ceiling_dbfs: -1.0 },
                _ => return Err(anyhow!("normalize must be none, peak or lufs")),
            };
        }
        match (&mut p.normalize, self.target, self.ceiling) {
            (Normalize::Peak { target_dbfs }, Some(t), _) => *target_dbfs = t,
            (Normalize::Lufs { target_lufs, true_peak_ceiling_dbfs }, t, c) => {
                if let Some(t) = t {
                    *target_lufs = t;
                }
                if let Some(c) = c {
                    *true_peak_ceiling_dbfs = c;
                }
            }
            (_, Some(_), _) | (_, _, Some(_)) => return Err(anyhow!("target and ceiling need peak or lufs normalisation")),
            _ => {}
        }
        if let Some(d) = self.dither {
            p.dither = d;
        }
        if let Some(f) = self.fade {
            p.fade_shape = f;
        }
        if let Some(t) = &self.template {
            p.path_template = t.clone();
        }
        Ok(())
    }
}

#[derive(Args)]
struct ExportArgs {
    file: PathBuf,
    #[arg(long)]
    preset: Option<String>,
    /// Selection start in seconds.
    #[arg(long = "in")]
    start: Option<f64>,
    /// Selection end in seconds.
    #[arg(long = "out")]
    end: Option<f64>,
    /// Shrink the selection, or the whole file, to its sound: what sits 40 dB under its peak goes.
    #[arg(long)]
    trim: bool,
    #[arg(long, default_value_t = 0.0)]
    fade_in: f64,
    #[arg(long, default_value_t = 0.0)]
    fade_out: f64,
    /// Level in dB, applied unless the preset normalises (--gain=-6 for a cut).
    #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
    gain: f64,
    /// Folder under the output root to write into; remembered for the next export.
    #[arg(long)]
    into: Option<PathBuf>,
    /// Full output path, replacing the preset's template.
    #[arg(long)]
    to: Option<PathBuf>,
    #[arg(long)]
    output_root: Option<PathBuf>,
    #[arg(long)]
    overwrite: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn parse_jump(s: &str) -> Result<(f64, f64), String> {
    let (at, to) = s.split_once(':').ok_or("expected AT:TO seconds")?;
    Ok((at.parse().map_err(|_| "bad AT")?, to.parse().map_err(|_| "bad TO")?))
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn abs(p: &Path) -> Result<PathBuf> {
    Ok(std::fs::canonicalize(p).with_context(|| format!("{}", p.display()))?)
}

/// Absolute path of an output folder, created when missing.
fn abs_dir(p: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(p).with_context(|| format!("create {}", p.display()))?;
    abs(p)
}

fn fmt_secs(s: f64) -> String {
    if s >= 60.0 {
        format!("{}:{:05.2}", (s / 60.0).floor() as u64, s % 60.0)
    } else {
        format!("{s:.3}s")
    }
}

fn print_records(records: &[hark_core::FileRecord], json: bool) -> Result<()> {
    if json {
        return print_json(&records);
    }
    for r in records {
        let info = r.info.as_ref();
        println!(
            "{}{:<40} {:>9} {:>2}ch {:>6} {:<5} {:<5} {}{}",
            if r.favorite { "★ " } else { "  " },
            r.name.chars().take(40).collect::<String>(),
            info.map(|i| fmt_secs(i.duration_secs)).unwrap_or_else(|| "?".into()),
            info.map(|i| i.channels).unwrap_or(0),
            info.map(|i| format!("{}k", i.sample_rate as f64 / 1000.0)).unwrap_or_default(),
            info.map(|i| i.codec.clone()).unwrap_or_default(),
            r.lufs.map(|l| format!("{l:.1}")).unwrap_or_default(),
            r.tags.join(","),
            r.error.as_ref().map(|e| format!("  [{e}]")).unwrap_or_default(),
        );
    }
    Ok(())
}

fn load(project: &Option<String>) -> Result<Config> {
    let mut config = Config::load()?;
    if let Some(p) = project {
        let path = match config.find_project(p) {
            Some(path) => path,
            None if Path::new(p).is_file() => abs(Path::new(p))?,
            None => return Err(anyhow!("no project {p}")),
        };
        let project = hark_core::Project::load(&path)?;
        config.active = Some(hark_core::config::ActiveProject { path, project });
    }
    Ok(config)
}

fn open_db(project: &Option<String>) -> Result<Db> {
    let config = load(project)?;
    let mut db = Db::open()?;
    db.set_project(config.project_id());
    Ok(db)
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;
    let project = cli.project.clone();
    match cli.cmd {
        Cmd::Projects { cmd } => {
            let mut config = load(&project)?;
            let db = open_db(&project)?;
            match cmd.unwrap_or(ProjectsCmd::List) {
                ProjectsCmd::List => {}
                ProjectsCmd::New { file, output } => {
                    config.create_project(&file)?;
                    if let Some(o) = output {
                        config.set_output_root(Some(abs_dir(&o)?))?;
                    }
                }
                ProjectsCmd::Open { project: key } => {
                    let path = config.find_project(&key).unwrap_or(PathBuf::from(&key));
                    config.open_project(&path)?;
                }
                ProjectsCmd::Close => config.close_project()?,
                ProjectsCmd::Forget { project: key } => {
                    let path = config.find_project(&key).ok_or_else(|| anyhow!("no project {key}"))?;
                    config.forget_project(&path)?;
                }
                ProjectsCmd::Output { dir } => config.set_output_root(Some(abs_dir(&dir)?))?,
            }
            let open = config.project.clone();
            let rows: Vec<serde_json::Value> = config
                .project_files
                .iter()
                .map(|path| {
                    let loaded = hark_core::Project::load(path).ok();
                    serde_json::json!({
                        "file": path,
                        "open": Some(path) == open.as_ref(),
                        "missing": loaded.is_none(),
                        "name": hark_core::project::name_of_path(path),
                        "id": loaded.as_ref().map(|p| p.id.clone()),
                        "output_root": loaded.as_ref().and_then(|p| p.output_root.clone()),
                        "roots": loaded.as_ref().map(|p| p.roots.clone()).unwrap_or_default(),
                        "favorites": loaded.as_ref().and_then(|p| db.favorite_count(&p.id).ok()).unwrap_or(0),
                    })
                })
                .collect();
            if json {
                print_json(&serde_json::json!({ "open": open, "default_roots": config.roots, "default_output_root": config.output_root, "default_favorites": db.favorite_count(hark_core::config::DEFAULT_PROJECT).unwrap_or(0), "projects": rows }))?;
            } else {
                println!(
                    "{} {:<16} {:<40} {} favorites, {} folders",
                    if open.is_none() { "*" } else { " " },
                    "Default",
                    config.output_root.as_ref().map(|o| o.display().to_string()).unwrap_or_else(|| "(no output folder)".into()),
                    db.favorite_count(hark_core::config::DEFAULT_PROJECT).unwrap_or(0),
                    config.roots.len()
                );
                for r in &rows {
                    let roots = r["roots"].as_array().map(|s| s.len()).unwrap_or(0);
                    println!(
                        "{} {:<16} {:<40} {} favorites, {} folders  {}{}",
                        if r["open"].as_bool() == Some(true) { "*" } else { " " },
                        r["name"].as_str().unwrap_or(""),
                        r["output_root"].as_str().unwrap_or("(no output folder)"),
                        r["favorites"],
                        roots,
                        r["file"].as_str().unwrap_or(""),
                        if r["missing"].as_bool() == Some(true) { "  (missing)" } else { "" }
                    );
                }
            }
        }
        Cmd::Roots { cmd } => {
            let mut config = load(&project)?;
            match cmd.unwrap_or(RootsCmd::List) {
                RootsCmd::List => {
                    if json {
                        print_json(&config.roots())?;
                    } else {
                        for r in config.roots() {
                            println!("{:<16} {}", r.name, r.path.display());
                        }
                    }
                }
                RootsCmd::Add { path, name } => {
                    let path = abs(&path)?;
                    let name = name.unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into()));
                    if config.roots().iter().any(|r| r.name == name || r.path == path) {
                        return Err(anyhow!("folder {name} is already in the project"));
                    }
                    config.roots_mut().push(Root { name: name.clone(), path: path.clone() });
                    config.save_target()?;
                    let mut db = open_db(&project)?;
                    let report = db.scan(&path, |_, _| {})?;
                    if json {
                        print_json(&report)?;
                    } else {
                        println!("added {name}: {} files", report.seen);
                    }
                }
                RootsCmd::Remove { name } => {
                    let Some(i) = config.roots().iter().position(|r| r.name == name) else { return Err(anyhow!("no folder {name}")) };
                    config.roots_mut().remove(i);
                    config.save_target()?;
                    println!("removed {name}");
                }
            }
        }
        Cmd::Scan { root } => {
            let config = load(&project)?;
            let mut db = open_db(&project)?;
            let roots: Vec<&Root> = match &root {
                Some(r) => vec![config.roots().iter().find(|x| &x.name == r || x.path == Path::new(r)).ok_or_else(|| anyhow!("no folder {r}"))?],
                None => config.roots().iter().collect(),
            };
            let mut reports = Vec::new();
            let mut failed = Vec::new();
            for r in roots {
                match db.scan(&r.path, |_, _| {}) {
                    Ok(report) => {
                        if !json {
                            println!("{}: {} seen, {} added, {} updated, {} removed, {} errors", r.name, report.seen, report.added, report.updated, report.removed, report.errors.len());
                            for (p, e) in &report.errors {
                                println!("  {}: {e}", p.display());
                            }
                        }
                        reports.push((r.name.clone(), report));
                    }
                    Err(e) => {
                        if !json {
                            eprintln!("{}: {e:#}", r.name);
                        }
                        failed.push((r.name.clone(), format!("{e:#}")));
                    }
                }
            }
            let hashed = db.match_orphaned_marks(|_, _| {})?;
            if json {
                print_json(&serde_json::json!({ "roots": reports, "failed": failed, "hashed": hashed }))?;
            } else if hashed > 0 {
                println!("{hashed} files hashed so that moved files keep their marks");
            }
        }
        Cmd::Ls { dir, sort } => {
            let records = open_db(&project)?.list_dir(&abs(&dir)?, sort.into())?;
            print_records(&records, json)?;
        }
        Cmd::Search { words, tag, fav, extension, channels, under, limit, sort } => {
            let config = load(&project)?;
            let q = Query {
                text: words.join(" "),
                dir: None,
                under: under.map(|u| abs(&u)).transpose()?,
                scope: config.roots().iter().map(|r| r.path.clone()).collect(),
                favorites_only: fav,
                tags: tag,
                extensions: extension,
                channels,
                sort: sort.into(),
                limit,
            };
            let records = open_db(&project)?.search(&q)?;
            print_records(&records, json)?;
        }
        Cmd::Inspect { file } => {
            let file = abs(&file)?;
            let info = hark_core::probe(&file).ok();
            let record = open_db(&project)?.get(&file)?;
            let origin = hark_core::origin::read(&file)?;
            let bext = hark_core::wav::read_bext_description(&file).ok().flatten();
            if json {
                print_json(&serde_json::json!({ "path": file, "info": info, "record": record, "origin": origin, "bext_description": bext }))?;
            } else {
                println!("{}", file.display());
                if let Some(i) = &info {
                    println!("  {} {} · {} Hz · {} ch · {} · {}", i.container, i.codec, i.sample_rate, i.channels, i.bits_per_sample.map(|b| format!("{b} bit")).unwrap_or_else(|| "float".into()), fmt_secs(i.duration_secs));
                }
                if let Some(r) = &record {
                    println!("  library: tags [{}]{}{}", r.tags.join(", "), if r.favorite { " ★" } else { "" }, r.lufs.map(|l| format!(" · {l:.1} LUFS")).unwrap_or_default());
                    if let Some(h) = &r.hash {
                        println!("  blake3: {h}");
                    }
                } else {
                    println!("  not in the library");
                }
                if let Some(d) = &bext {
                    println!("  bext: {d}");
                }
                if let Some(o) = &origin {
                    println!("  origin: {}", o.summary());
                    println!("    exported {} by hark {} with preset {}", o.exported_at, o.hark, o.preset_name.as_deref().unwrap_or("-"));
                    println!("    source {} ({} bytes, blake3 {})", o.source.path.display(), o.source.size, &o.source.blake3[..16]);
                    println!("    fades {:.3}s / {:.3}s · gain {:+.2} dB{}", o.fade_in_secs, o.fade_out_secs, o.measured.applied_gain_db, if o.measured.limited_by_true_peak { " (true-peak limited)" } else { "" });
                }
            }
        }
        Cmd::Analyze { file, store } => {
            let file = abs(&file)?;
            let l = hark_core::loudness::measure_file(&file)?;
            if store {
                open_db(&project)?.set_loudness(&file, l.integrated_lufs, l.true_peak_dbfs)?;
            }
            if json {
                print_json(&l)?;
            } else {
                println!("{:.2} LUFS{} · true peak {:.2} dBTP · sample peak {:.2} dBFS{}", l.integrated_lufs, if l.momentary { " (momentary max)" } else { "" }, l.true_peak_dbfs, l.sample_peak_dbfs, l.range_lu.map(|r| format!(" · range {r:.1} LU")).unwrap_or_default());
            }
        }
        Cmd::Takes { file } => {
            let file = abs(&file)?;
            let takes = hark_core::decode(&file)?.takes();
            if json {
                print_json(&takes.iter().map(|(a, b)| serde_json::json!({ "start_secs": a, "end_secs": b })).collect::<Vec<_>>())?;
            } else {
                for (k, (a, b)) in takes.iter().enumerate() {
                    println!("{:>3}  {a:.3} - {b:.3}  ({:.3} s)", k + 1, b - a);
                }
            }
        }
        Cmd::Tag { file, changes } => {
            let file = abs(&file)?;
            let db = open_db(&project)?;
            let hash = db.ensure_hash(&file)?;
            let config = load(&project)?;
            for c in &changes {
                let (on, name) = match c.chars().next() {
                    Some('+') => (true, &c[1..]),
                    Some('-') => (false, &c[1..]),
                    _ => (true, c.as_str()),
                };
                if !config.tags.iter().any(|t| t.name == name) {
                    eprintln!("note: {name} is not a defined tag; applied anyway");
                }
                db.set_tag(&hash, name, on)?;
            }
            let record = db.get(&file)?;
            if json {
                print_json(&record)?;
            } else if let Some(r) = record {
                println!("{}: [{}]", r.name, r.tags.join(", "));
            }
        }
        Cmd::Fav { file, off } => {
            let file = abs(&file)?;
            let db = open_db(&project)?;
            let hash = db.ensure_hash(&file)?;
            db.set_favorite(&hash, !off)?;
            println!("{}: {}", file.display(), if off { "unmarked" } else { "★" });
        }
        Cmd::Tags { cmd } => {
            let mut config = load(&project)?;
            let db = open_db(&project)?;
            let find = |config: &Config, name: &str| config.tags.iter().position(|t| t.name == name).ok_or_else(|| anyhow!("no tag {name}"));
            match cmd.unwrap_or(TagsCmd::List) {
                TagsCmd::List => {}
                TagsCmd::Add { name, color } => {
                    if config.tags.iter().any(|t| t.name == name) {
                        return Err(anyhow!("tag {name} exists"));
                    }
                    config.tags.push(hark_core::TagDef { name, color });
                    config.save()?;
                }
                TagsCmd::Set { name, color } => {
                    let i = find(&config, &name)?;
                    if let Some(c) = color {
                        config.tags[i].color = if c.is_empty() { None } else { Some(c) };
                    }
                    config.save()?;
                }
                TagsCmd::Rename { old, new } => {
                    let i = find(&config, &old)?;
                    if config.tags.iter().any(|t| t.name == new) {
                        return Err(anyhow!("tag {new} exists"));
                    }
                    config.tags[i].name = new.clone();
                    config.save()?;
                    let n = db.rename_tag(&old, &new)?;
                    if !json {
                        println!("renamed {old} to {new} on {n} files");
                    }
                }
                TagsCmd::Move { name, position } => {
                    let i = find(&config, &name)?;
                    let t = config.tags.remove(i);
                    let at = position.min(config.tags.len());
                    config.tags.insert(at, t);
                    config.save()?;
                }
                TagsCmd::Remove { name } => {
                    let i = find(&config, &name)?;
                    config.tags.remove(i);
                    config.save()?;
                    let n = db.remove_tag(&name)?;
                    if !json {
                        println!("removed {name} from {n} files");
                    }
                }
            }
            let used = db.tags_in_use()?;
            if json {
                print_json(&serde_json::json!({ "defined": config.tags, "in_use": used }))?;
            } else {
                for (k, t) in config.tags.iter().enumerate() {
                    let n = used.iter().find(|(u, _)| u == &t.name).map(|(_, n)| *n).unwrap_or(0);
                    println!("{:>2} {:<12} {:>5} files  {}", k + 1, t.name, n, t.color.as_deref().unwrap_or("-"));
                }
                for (u, n) in used.iter().filter(|(u, _)| !config.tags.iter().any(|t| &t.name == u)) {
                    println!("{u:<12} {n:>5} files  (undefined)");
                }
            }
        }
        Cmd::Keys { cmd } => {
            use hark_core::keys::Action;
            let mut config = load(&project)?;
            let find = |id: &str| Action::from_id(id).ok_or_else(|| anyhow!("no action {id}; one of {}", Action::ALL.map(Action::id).join(", ")));
            match cmd.unwrap_or(KeysCmd::List) {
                KeysCmd::List => {
                    if json {
                        let rows: Vec<serde_json::Value> = Action::ALL
                            .into_iter()
                            .map(|a| serde_json::json!({ "action": a.id(), "label": a.label(), "chord": config.keys.chord(a), "default": a.default_chord() }))
                            .collect();
                        print_json(&rows)?;
                    } else {
                        for a in Action::ALL {
                            let chord = config.keys.chord(a);
                            println!("{:<16} {:<14} {}{}", a.id(), chord, a.label(), if chord == a.default_chord() { String::new() } else { format!("  (default {})", a.default_chord()) });
                        }
                    }
                }
                KeysCmd::Set { action, chord } => {
                    let a = find(&action)?;
                    config.keys.set(a, &chord).map_err(|e| anyhow!("{e}"))?;
                    config.save()?;
                    println!("{} = {}", a.id(), config.keys.chord(a));
                }
                KeysCmd::Reset { action } => {
                    match action {
                        Some(id) => config.keys.reset(find(&id)?),
                        None => config.keys.reset_all(),
                    }
                    config.save()?;
                }
            }
        }
        Cmd::Presets { cmd } => {
            let mut config = load(&project)?;
            match cmd.unwrap_or(PresetsCmd::List) {
                PresetsCmd::List => {
                    if json {
                        print_json(&config.presets)?;
                    } else {
                        for (name, p) in &config.presets {
                            println!(
                                "{:<10} {} {}bit {} {} {:?} → {}{}",
                                name,
                                p.format.extension(),
                                p.bits,
                                p.sample_rate.map(|r| format!("{r} Hz")).unwrap_or_else(|| "keep rate".into()),
                                if p.mono.is_some() { "mono" } else { "keep ch" },
                                p.normalize,
                                p.path_template,
                                if name == &config.default_preset { "  (default)" } else { "" }
                            );
                        }
                    }
                }
                PresetsCmd::Show { name } => print_json(config.preset(&name).ok_or_else(|| anyhow!("no preset {name}"))?)?,
                PresetsCmd::Add { name, from } => {
                    if config.presets.contains_key(&name) {
                        return Err(anyhow!("preset {name} exists"));
                    }
                    let values = match from {
                        Some(f) => config.preset(&f).ok_or_else(|| anyhow!("no preset {f}"))?.clone(),
                        None => hark_core::ExportPreset::default(),
                    };
                    config.presets.insert(name.clone(), values);
                    config.save()?;
                    print_json(&config.preset(&name))?;
                }
                PresetsCmd::Set { name, values } => {
                    let mut p = config.preset(&name).ok_or_else(|| anyhow!("no preset {name}"))?.clone();
                    values.apply(&mut p)?;
                    config.presets.insert(name.clone(), p);
                    config.save()?;
                    print_json(&config.preset(&name))?;
                }
                PresetsCmd::Rename { old, new } => {
                    config.rename_preset(&old, &new)?;
                    config.save()?;
                }
                PresetsCmd::Remove { name } => {
                    config.remove_preset(&name)?;
                    config.save()?;
                }
            }
        }
        Cmd::Export(a) => {
            let mut config = load(&project)?;
            if let Some(o) = &a.output_root {
                let root = Some(abs_dir(o)?);
                match config.active.as_mut() {
                    Some(active) => active.project.output_root = root,
                    None => config.output_root = root,
                }
            }
            let file = abs(&a.file)?;
            let db = open_db(&project)?;
            let record = db.get(&file)?;
            let (tags, favorite) = record.map(|r| (r.tags, r.favorite)).unwrap_or_default();
            let mut req = ExportRequest::from_config(&config, &file, a.preset.as_deref(), tags, favorite)?;
            req.selection = match (a.start, a.end) {
                (None, None) => None,
                (s, e) => Some(Selection { start_secs: s.unwrap_or(0.0), end_secs: e.unwrap_or(f64::MAX) }),
            };
            if let Some(sel) = &mut req.selection {
                if sel.end_secs == f64::MAX {
                    sel.end_secs = hark_core::probe(&file)?.duration_secs;
                }
            }
            req.trim = a.trim;
            req.fade_in_secs = a.fade_in;
            req.fade_out_secs = a.fade_out;
            req.gain_db = a.gain;
            if let Some(into) = &a.into {
                let into = into.strip_prefix(&req.output_root).map(|p| p.to_path_buf()).unwrap_or_else(|_| into.clone());
                if into.is_absolute() {
                    return Err(anyhow!("--into must be a folder under the output root"));
                }
                req.subfolder = Some(into.clone());
                config.set_export_folder(Some(into))?;
            }
            req.output_path = a.to;
            req.overwrite = a.overwrite;
            let result = hark_core::export::run(&req)?;
            report_export(&result, json)?;
        }
        Cmd::Copies { cmd } => {
            let mut config = load(&project)?;
            match cmd.unwrap_or(CopiesCmd::List) {
                CopiesCmd::List => {
                    let (summary, files) = hark_core::copies::list(&config.copy_folder());
                    if json {
                        print_json(&serde_json::json!({ "folder": summary.folder, "files": summary.files, "bytes": summary.bytes, "paths": files }))?;
                    } else {
                        println!("{}  ·  {} files  ·  {:.1} MB", summary.folder.display(), summary.files, summary.bytes as f64 / 1e6);
                        for f in &files {
                            println!("  {}", f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                        }
                    }
                }
                CopiesCmd::Clear => {
                    let folder = config.copy_folder();
                    let n = hark_core::copies::clear(&folder)?;
                    if json {
                        print_json(&serde_json::json!({ "folder": folder, "removed": n }))?;
                    } else {
                        println!("removed {n} files from {}", folder.display());
                    }
                }
                CopiesCmd::Folder { dir, default, unset } => {
                    let value = match (&dir, unset) {
                        (Some(d), _) => Some(Some(abs_dir(d)?)),
                        (None, true) => Some(None),
                        (None, false) => None,
                    };
                    if let Some(value) = value {
                        match config.active.as_mut().filter(|_| !default) {
                            Some(active) => {
                                active.project.copy_folder = value;
                                active.project.save(&active.path)?;
                            }
                            None => {
                                config.copy_folder = value;
                                config.save()?;
                            }
                        }
                    }
                    let project_folder = config.active.as_ref().and_then(|a| a.project.copy_folder.clone());
                    if json {
                        print_json(&serde_json::json!({ "folder": config.copy_folder(), "project": project_folder, "default": config.copy_folder, "fallback": hark_core::paths::copies_dir() }))?;
                    } else {
                        println!("{}", config.copy_folder().display());
                    }
                }
            }
        }
        Cmd::Reexport { files, preset, output_root, in_place } => {
            let config = load(&project)?;
            let mut results = Vec::new();
            for f in &files {
                let f = abs(f)?;
                let origin = hark_core::origin::read(&f)?.ok_or_else(|| anyhow!("{}: carries no hark origin", f.display()))?;
                let root = match &output_root {
                    Some(o) => abs_dir(o)?,
                    None => config.output_root().map(|p| p.to_path_buf()).ok_or_else(|| anyhow!("{} has no output folder", config.project_name()))?,
                };
                let mut req = hark_core::export::request_from_origin(&origin, &root, if in_place { Some(f.clone()) } else { None });
                if let Some(p) = &preset {
                    req.preset = config.preset(p).ok_or_else(|| anyhow!("no preset {p}"))?.clone();
                    req.preset_name = Some(p.clone());
                }
                let result = hark_core::export::run(&req)?;
                if !json {
                    report_export(&result, false)?;
                }
                results.push(result);
            }
            if json {
                print_json(&results)?;
            }
        }
        Cmd::Config { output_root, default_preset, ui_zoom, audition_rate, stream_above_mb, path } => {
            let mut config = load(&project)?;
            let mut changed = false;
            if let Some(o) = output_root {
                config.set_output_root(Some(abs_dir(&o)?))?;
            }
            if let Some(p) = default_preset {
                if !config.presets.contains_key(&p) {
                    return Err(anyhow!("no preset {p}"));
                }
                config.default_preset = p;
                changed = true;
            }
            if let Some(z) = ui_zoom {
                config.ui_zoom = z.clamp(0.5, 3.0);
                changed = true;
            }
            if let Some(r) = audition_rate {
                config.audition_sample_rate = (r > 0).then_some(r);
                changed = true;
            }
            if let Some(mb) = stream_above_mb {
                config.stream_above_mb = mb.max(0.0);
                changed = true;
            }
            if changed {
                config.save()?;
            }
            if path {
                println!("{}", hark_core::paths::config_file().display());
            } else {
                print_json(&config)?;
            }
        }
        Cmd::Play { file, start, end, from, looped, seconds, jump, stream_above } => {
            let file = abs(&file)?;
            let config = load(&project)?;
            let player = hark_core::audition::Player::open(config.audition_sample_rate)?;
            let info = hark_core::probe(&file)?;
            let streamed = info.decoded_mb() > stream_above.unwrap_or(config.stream_above_mb);
            let source = if streamed {
                Source::File { path: file.clone(), source_rate: info.sample_rate, channels: info.channels, source_frames: (info.duration_secs * info.sample_rate as f64).round() as u64 }
            } else {
                let pcm = hark_core::decode(&file)?;
                Source::Whole(std::sync::Arc::new(hark_core::resample::resample(&pcm, player.sample_rate)?))
            };
            let rate = player.sample_rate as f64;
            let total = source.frames(player.sample_rate);
            let frame = |secs: f64| ((secs * rate).round().max(0.0) as usize).min(total);
            let clip = Clip {
                source,
                start: start.map(frame).unwrap_or(0),
                end: end.map(frame).unwrap_or(total),
                looped,
                fade_in: 0,
                fade_out: 0,
                fade_shape: Default::default(),
            };
            let from = from.map(frame).unwrap_or(clip.start);
            if !json {
                eprintln!("{} on {} at {} Hz", if streamed { "streaming" } else { "playing" }, player.device_name, player.sample_rate);
            }
            let t0 = Instant::now();
            player.play_from(clip.clone(), from);
            let mut first_audio = None;
            let mut jumped = false;
            let tick = Duration::from_millis(1);
            loop {
                let played = t0.elapsed().as_secs_f64();
                if first_audio.is_none() && player.position() > from {
                    first_audio = Some(played);
                }
                if let Some((at, to)) = jump {
                    if !jumped && played >= at {
                        player.play_from(clip.clone(), frame(to));
                        jumped = true;
                    }
                }
                if seconds.map(|s| played >= s).unwrap_or(false) {
                    player.stop();
                    std::thread::sleep(Duration::from_millis(20));
                    break;
                }
                if played > 0.05 && !player.is_playing() {
                    break;
                }
                std::thread::sleep(tick);
            }
            let report = serde_json::json!({
                "streamed": streamed,
                "device_rate": player.sample_rate,
                "first_audio_ms": first_audio.map(|s| s * 1000.0),
                "played_secs": t0.elapsed().as_secs_f64(),
                "position_frames": player.position(),
                "underrun_frames": player.underrun_frames(),
            });
            if json {
                print_json(&report)?;
            } else {
                eprintln!("first audio after {:.1} ms, {} underrun frames", first_audio.unwrap_or(f64::NAN) * 1000.0, player.underrun_frames());
            }
        }
    }
    Ok(())
}

fn report_export(result: &hark_core::export::ExportResult, json: bool) -> Result<()> {
    if json {
        return print_json(result);
    }
    let o = &result.origin;
    println!(
        "{} ← {} · {} {}bit {} Hz {}ch · {} · gain {:+.2} dB",
        result.output.display(),
        o.source.file_name,
        o.output.format,
        o.output.bits,
        o.output.sample_rate,
        o.output.channels,
        fmt_secs(o.output.duration_secs),
        o.measured.applied_gain_db
    );
    for w in &result.warnings {
        println!("  warning: {w}");
    }
    Ok(())
}
