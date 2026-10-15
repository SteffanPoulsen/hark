use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use hark_core::config::Root;
use hark_core::export::{ExportRequest, Selection};
use hark_core::library::Query;
use hark_core::{Config, Db};

#[derive(Parser)]
#[command(name = "hark-cli", version, about = "hark without the window: library, marks, analysis and export")]
struct Cli {
    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Library roots.
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
    Ls { dir: PathBuf },
    /// Search the library by path words, tag and favorite.
    Search {
        words: Vec<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        fav: bool,
        #[arg(long)]
        under: Option<PathBuf>,
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// Everything known about a file: probe, library record, and the origin it carries if hark exported it.
    Inspect { file: PathBuf },
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
    /// Export a file, or a selection of it, through a preset.
    Export(ExportArgs),
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
        /// Print the config file path.
        #[arg(long)]
        path: bool,
    },
    /// Play a file through the audition engine and wait for it to finish.
    Play {
        file: PathBuf,
        #[arg(long = "in")]
        start: Option<f64>,
        #[arg(long = "out")]
        end: Option<f64>,
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
enum TagsCmd {
    List,
    Add {
        name: String,
        #[arg(long)]
        color: Option<String>,
        #[arg(long)]
        preset: Option<String>,
    },
    /// Change a tag's color or preset; an empty value clears it.
    Set {
        name: String,
        #[arg(long)]
        color: Option<String>,
        #[arg(long)]
        preset: Option<String>,
    },
    /// Rename a tag and rewrite the marks that carry it.
    Rename { old: String, new: String },
    /// Move a tag to a position (0-based); position decides the 1-9 hotkey.
    Move { name: String, position: usize },
    /// Remove a tag definition and the marks that carry it.
    Remove { name: String },
}

#[derive(Subcommand)]
enum PresetsCmd {
    List,
    Show { name: String },
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
    #[arg(long, default_value_t = 0.0)]
    fade_in: f64,
    #[arg(long, default_value_t = 0.0)]
    fade_out: f64,
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

fn run() -> Result<()> {
    let cli = Cli::parse();
    let json = cli.json;
    match cli.cmd {
        Cmd::Roots { cmd } => {
            let mut config = Config::load()?;
            match cmd.unwrap_or(RootsCmd::List) {
                RootsCmd::List => {
                    if json {
                        print_json(&config.roots)?;
                    } else {
                        for r in &config.roots {
                            println!("{:<16} {}", r.name, r.path.display());
                        }
                    }
                }
                RootsCmd::Add { path, name } => {
                    let path = abs(&path)?;
                    let name = name.unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into()));
                    if config.roots.iter().any(|r| r.name == name) {
                        return Err(anyhow!("root {name} exists"));
                    }
                    config.roots.push(Root { name: name.clone(), path: path.clone() });
                    config.save()?;
                    let mut db = Db::open()?;
                    let report = db.scan(&path, |_, _| {})?;
                    if json {
                        print_json(&report)?;
                    } else {
                        println!("added {name}: {} files", report.seen);
                    }
                }
                RootsCmd::Remove { name } => {
                    let Some(i) = config.roots.iter().position(|r| r.name == name) else { return Err(anyhow!("no root {name}")) };
                    let root = config.roots.remove(i);
                    config.save()?;
                    Db::open()?.forget_root(&root.path)?;
                    println!("removed {name}");
                }
            }
        }
        Cmd::Scan { root } => {
            let config = Config::load()?;
            let mut db = Db::open()?;
            let roots: Vec<&Root> = match &root {
                Some(r) => vec![config.roots.iter().find(|x| &x.name == r || x.path == Path::new(r)).ok_or_else(|| anyhow!("no root {r}"))?],
                None => config.roots.iter().collect(),
            };
            let mut reports = Vec::new();
            for r in roots {
                let report = db.scan(&r.path, |_, _| {})?;
                if !json {
                    println!("{}: {} seen, {} added, {} updated, {} removed, {} errors", r.name, report.seen, report.added, report.updated, report.removed, report.errors.len());
                    for (p, e) in &report.errors {
                        println!("  {}: {e}", p.display());
                    }
                }
                reports.push((r.name.clone(), report));
            }
            if json {
                print_json(&reports)?;
            }
        }
        Cmd::Ls { dir } => {
            let records = Db::open()?.list_dir(&abs(&dir)?)?;
            print_records(&records, json)?;
        }
        Cmd::Search { words, tag, fav, under, limit } => {
            let q = Query { text: words.join(" "), tag, favorites_only: fav, under: under.map(|u| abs(&u)).transpose()?, limit };
            let records = Db::open()?.search(&q)?;
            print_records(&records, json)?;
        }
        Cmd::Inspect { file } => {
            let file = abs(&file)?;
            let info = hark_core::probe(&file).ok();
            let record = Db::open()?.get(&file)?;
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
            let pcm = hark_core::decode(&file)?;
            let l = hark_core::loudness::measure(&pcm)?;
            if store {
                Db::open()?.set_loudness(&file, l.integrated_lufs, l.true_peak_dbfs)?;
            }
            if json {
                print_json(&l)?;
            } else {
                println!("{:.2} LUFS{} · true peak {:.2} dBTP · sample peak {:.2} dBFS{}", l.integrated_lufs, if l.momentary { " (momentary max)" } else { "" }, l.true_peak_dbfs, l.sample_peak_dbfs, l.range_lu.map(|r| format!(" · range {r:.1} LU")).unwrap_or_default());
            }
        }
        Cmd::Tag { file, changes } => {
            let file = abs(&file)?;
            let db = Db::open()?;
            let hash = db.ensure_hash(&file)?;
            let config = Config::load()?;
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
            let db = Db::open()?;
            let hash = db.ensure_hash(&file)?;
            db.set_favorite(&hash, !off)?;
            println!("{}: {}", file.display(), if off { "unmarked" } else { "★" });
        }
        Cmd::Tags { cmd } => {
            let mut config = Config::load()?;
            let db = Db::open()?;
            let find = |config: &Config, name: &str| config.tags.iter().position(|t| t.name == name).ok_or_else(|| anyhow!("no tag {name}"));
            match cmd.unwrap_or(TagsCmd::List) {
                TagsCmd::List => {}
                TagsCmd::Add { name, color, preset } => {
                    if config.tags.iter().any(|t| t.name == name) {
                        return Err(anyhow!("tag {name} exists"));
                    }
                    config.tags.push(hark_core::TagDef { name, color, preset });
                    config.save()?;
                }
                TagsCmd::Set { name, color, preset } => {
                    let i = find(&config, &name)?;
                    if let Some(c) = color {
                        config.tags[i].color = if c.is_empty() { None } else { Some(c) };
                    }
                    if let Some(p) = preset {
                        config.tags[i].preset = if p.is_empty() { None } else { Some(p) };
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
                    println!("{:>2} {:<12} {:>5} files  {:<8} preset {}", k + 1, t.name, n, t.color.as_deref().unwrap_or("-"), t.preset.as_deref().unwrap_or("-"));
                }
                for (u, n) in used.iter().filter(|(u, _)| !config.tags.iter().any(|t| &t.name == u)) {
                    println!("{u:<12} {n:>5} files  (undefined)");
                }
            }
        }
        Cmd::Presets { cmd } => {
            let config = Config::load()?;
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
            }
        }
        Cmd::Export(a) => {
            let mut config = Config::load()?;
            if let Some(o) = &a.output_root {
                config.output_root = Some(abs_dir(o)?);
            }
            let file = abs(&a.file)?;
            let db = Db::open()?;
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
            req.fade_in_secs = a.fade_in;
            req.fade_out_secs = a.fade_out;
            req.output_path = a.to;
            req.overwrite = a.overwrite;
            let result = hark_core::export::run(&req)?;
            report_export(&result, json)?;
        }
        Cmd::Reexport { files, preset, output_root, in_place } => {
            let config = Config::load()?;
            let mut results = Vec::new();
            for f in &files {
                let f = abs(f)?;
                let origin = hark_core::origin::read(&f)?.ok_or_else(|| anyhow!("{}: carries no hark origin", f.display()))?;
                let root = match &output_root {
                    Some(o) => abs_dir(o)?,
                    None => config.output_root.clone().ok_or_else(|| anyhow!("no output root configured"))?,
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
        Cmd::Config { output_root, default_preset, path } => {
            let mut config = Config::load()?;
            let mut changed = false;
            if let Some(o) = output_root {
                config.output_root = Some(abs_dir(&o)?);
                changed = true;
            }
            if let Some(p) = default_preset {
                if !config.presets.contains_key(&p) {
                    return Err(anyhow!("no preset {p}"));
                }
                config.default_preset = p;
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
        Cmd::Play { file, start, end } => {
            let file = abs(&file)?;
            let player = hark_core::audition::Player::open(None)?;
            let pcm = hark_core::decode(&file)?;
            let pcm = hark_core::resample::resample(&pcm, player.sample_rate)?;
            let frames = pcm.frames();
            let clip = hark_core::audition::Clip {
                start: start.map(|s| pcm.frame_at(s)).unwrap_or(0),
                end: end.map(|s| pcm.frame_at(s)).unwrap_or(frames),
                ..hark_core::audition::Clip::whole(std::sync::Arc::new(pcm))
            };
            eprintln!("playing on {} at {} Hz", player.device_name, player.sample_rate);
            player.play(clip);
            std::thread::sleep(std::time::Duration::from_millis(50));
            while player.is_playing() {
                std::thread::sleep(std::time::Duration::from_millis(20));
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
