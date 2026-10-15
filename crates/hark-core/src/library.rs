//! The library database: every audio file under the roots with its probed metadata, and the
//! marks (tags, favorites) keyed by content hash so they follow a file through renames.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::decode::{is_audio_extension, probe, AudioInfo};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileRecord {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub mtime_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<AudioInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lufs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub true_peak_dbfs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub favorite: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanReport {
    pub seen: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub errors: Vec<(PathBuf, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelClass {
    Mono,
    Stereo,
    Multi,
}

impl ChannelClass {
    pub const ALL: [ChannelClass; 3] = [ChannelClass::Mono, ChannelClass::Stereo, ChannelClass::Multi];

    pub fn name(self) -> &'static str {
        match self {
            ChannelClass::Mono => "mono",
            ChannelClass::Stereo => "stereo",
            ChannelClass::Multi => "multi",
        }
    }
}

impl std::str::FromStr for ChannelClass {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "mono" | "1" => Ok(ChannelClass::Mono),
            "stereo" | "2" => Ok(ChannelClass::Stereo),
            "multi" => Ok(ChannelClass::Multi),
            _ => Err(format!("{s}: expected mono, stereo or multi")),
        }
    }
}

/// Every field narrows the result; an empty list or `None` leaves its axis open.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Words that must all appear in the path.
    pub text: String,
    /// Only files directly inside this folder.
    pub dir: Option<PathBuf>,
    /// Only files anywhere below this folder.
    pub under: Option<PathBuf>,
    /// Folders to search within; empty means everywhere.
    pub scope: Vec<PathBuf>,
    pub favorites_only: bool,
    /// Files carrying any of these tags.
    pub tags: Vec<String>,
    /// Lowercase extensions without the dot.
    pub extensions: Vec<String>,
    pub channels: Vec<ChannelClass>,
    pub limit: usize,
}

pub struct Db {
    conn: Connection,
    /// Favorites are read and written for this project.
    project: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files (
    path TEXT PRIMARY KEY,
    dir TEXT NOT NULL,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    hash TEXT,
    duration REAL,
    sample_rate INTEGER,
    channels INTEGER,
    bits INTEGER,
    codec TEXT,
    container TEXT,
    lufs REAL,
    true_peak REAL,
    error TEXT
);
CREATE INDEX IF NOT EXISTS files_dir ON files(dir);
CREATE INDEX IF NOT EXISTS files_hash ON files(hash);
CREATE INDEX IF NOT EXISTS files_name ON files(name COLLATE NOCASE);
CREATE TABLE IF NOT EXISTS file_tags (hash TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (hash, tag));
CREATE TABLE IF NOT EXISTS favorites (project TEXT NOT NULL, hash TEXT NOT NULL, PRIMARY KEY (project, hash));
";

const FAVORITES_BY_PROJECT: &str = "
ALTER TABLE favorites RENAME TO favorites_old;
CREATE TABLE favorites (project TEXT NOT NULL, hash TEXT NOT NULL, PRIMARY KEY (project, hash));
INSERT INTO favorites (project, hash) SELECT 'default', hash FROM favorites_old;
DROP TABLE favorites_old;
";

const SELECT: &str = "SELECT f.path, f.name, f.size, f.mtime_ns, f.hash, f.duration, f.sample_rate, f.channels, f.bits, f.codec, f.container, f.lufs, f.true_peak, f.error,
    (SELECT group_concat(tag, char(31)) FROM file_tags t WHERE t.hash = f.hash),
    EXISTS(SELECT 1 FROM favorites v WHERE v.hash = f.hash AND v.project = :project)
    FROM files f";

impl Db {
    pub fn open() -> Result<Db> {
        Self::open_at(&crate::paths::db_file())
    }

    pub fn open_at(path: &Path) -> Result<Db> {
        crate::paths::ensure_parent(path)?;
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Self::init(&conn)?;
        Ok(Db { conn, project: crate::config::DEFAULT_PROJECT.to_string() })
    }

    pub fn open_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Db { conn, project: crate::config::DEFAULT_PROJECT.to_string() })
    }

    fn init(conn: &Connection) -> Result<()> {
        conn.execute_batch(SCHEMA)?;
        let has_project: bool = conn
            .prepare("PRAGMA table_info(favorites)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|c| c == "project");
        if !has_project {
            conn.execute_batch(FAVORITES_BY_PROJECT)?;
        }
        Ok(())
    }

    pub fn set_project(&mut self, name: &str) {
        self.project = name.to_string();
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
        let sample_rate: Option<u32> = r.get(6)?;
        let info = match (r.get::<_, Option<f64>>(5)?, sample_rate) {
            (Some(duration_secs), Some(sample_rate)) => Some(AudioInfo {
                duration_secs,
                sample_rate,
                channels: r.get::<_, Option<u16>>(7)?.unwrap_or(0),
                bits_per_sample: r.get(8)?,
                codec: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                container: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
            }),
            _ => None,
        };
        let tags: Vec<String> = r
            .get::<_, Option<String>>(14)?
            .map(|s| s.split('\u{1f}').map(str::to_string).collect())
            .unwrap_or_default();
        Ok(FileRecord {
            path: PathBuf::from(r.get::<_, String>(0)?),
            name: r.get(1)?,
            size: r.get::<_, i64>(2)? as u64,
            mtime_ns: r.get(3)?,
            hash: r.get(4)?,
            info,
            lufs: r.get(11)?,
            true_peak_dbfs: r.get(12)?,
            error: r.get(13)?,
            tags,
            favorite: r.get::<_, bool>(15)?,
        })
    }

    pub fn get(&self, path: &Path) -> Result<Option<FileRecord>> {
        let sql = format!("{SELECT} WHERE f.path = :path");
        let path = path.to_string_lossy().into_owned();
        Ok(self.conn.query_row(&sql, &[(":project", &self.project as &dyn rusqlite::ToSql), (":path", &path)], Self::row).optional()?)
    }

    /// Files directly inside `dir`, by name.
    pub fn list_dir(&self, dir: &Path) -> Result<Vec<FileRecord>> {
        self.search(&Query { dir: Some(dir.to_path_buf()), ..Query::default() })
    }

    pub fn search(&self, q: &Query) -> Result<Vec<FileRecord>> {
        let mut sql = format!("{SELECT} WHERE 1=1");
        let mut names: Vec<String> = Vec::new();
        let mut values: Vec<String> = Vec::new();
        let bind = |names: &mut Vec<String>, values: &mut Vec<String>, v: String| -> String {
            let name = format!(":p{}", values.len());
            names.push(name.clone());
            values.push(v);
            name
        };
        let any_of = |names: &mut Vec<String>, values: &mut Vec<String>, items: &[String], clause: &dyn Fn(&str) -> String| -> String {
            let ors: Vec<String> = items.iter().map(|v| clause(&bind(names, values, v.clone()))).collect();
            format!(" AND ({})", ors.join(" OR "))
        };
        for word in q.text.split_whitespace() {
            let n = bind(&mut names, &mut values, format!("%{}%", word.replace('%', "\\%")));
            sql.push_str(&format!(" AND f.path LIKE {n} ESCAPE '\\'"));
        }
        if let Some(dir) = &q.dir {
            let n = bind(&mut names, &mut values, dir.to_string_lossy().into_owned());
            sql.push_str(&format!(" AND f.dir = {n}"));
        }
        if !q.tags.is_empty() {
            sql.push_str(&any_of(&mut names, &mut values, &q.tags, &|n| format!("EXISTS(SELECT 1 FROM file_tags t WHERE t.hash = f.hash AND t.tag = {n})")));
        }
        if q.favorites_only {
            sql.push_str(" AND EXISTS(SELECT 1 FROM favorites v WHERE v.hash = f.hash AND v.project = :project)");
        }
        if !q.extensions.is_empty() {
            let patterns: Vec<String> = q.extensions.iter().map(|e| format!("%.{}", e.to_lowercase())).collect();
            sql.push_str(&any_of(&mut names, &mut values, &patterns, &|n| format!("lower(f.name) LIKE {n}")));
        }
        if !q.channels.is_empty() {
            let ors: Vec<&str> = q
                .channels
                .iter()
                .map(|c| match c {
                    ChannelClass::Mono => "f.channels = 1",
                    ChannelClass::Stereo => "f.channels = 2",
                    ChannelClass::Multi => "f.channels > 2",
                })
                .collect();
            sql.push_str(&format!(" AND ({})", ors.join(" OR ")));
        }
        if let Some(under) = &q.under {
            let n = bind(&mut names, &mut values, format!("{}{}%", under.to_string_lossy(), std::path::MAIN_SEPARATOR));
            sql.push_str(&format!(" AND f.path LIKE {n}"));
        }
        if !q.scope.is_empty() {
            let prefixes: Vec<String> = q.scope.iter().map(|s| format!("{}{}%", s.to_string_lossy(), std::path::MAIN_SEPARATOR)).collect();
            sql.push_str(&any_of(&mut names, &mut values, &prefixes, &|n| format!("f.path LIKE {n}")));
        }
        sql.push_str(" ORDER BY f.name COLLATE NOCASE");
        if q.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", q.limit));
        }
        let mut params: Vec<(&str, &dyn rusqlite::ToSql)> = vec![(":project", &self.project)];
        for (n, v) in names.iter().zip(values.iter()) {
            params.push((n.as_str(), v));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), Self::row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn count_dir(&self, dir: &Path) -> Result<usize> {
        Ok(self.conn.query_row("SELECT count(*) FROM files WHERE dir = ?1", params![dir.to_string_lossy()], |r| r.get::<_, i64>(0))? as usize)
    }

    /// Walks `root`, probes new and changed files, and drops rows whose file is gone.
    pub fn scan(&mut self, root: &Path, mut progress: impl FnMut(&Path, usize)) -> Result<ScanReport> {
        let mut report = ScanReport::default();
        let prefix = format!("{}{}", root.to_string_lossy(), std::path::MAIN_SEPARATOR);
        let mut known: std::collections::HashMap<String, (u64, i64)> = {
            let mut stmt = self.conn.prepare("SELECT path, size, mtime_ns FROM files WHERE path LIKE ?1")?;
            let rows = stmt.query_map(params![format!("{prefix}%")], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)?))))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let tx = self.conn.transaction()?;
        for entry in walkdir::WalkDir::new(root).follow_links(true).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
            if !is_audio_extension(ext) {
                continue;
            }
            report.seen += 1;
            progress(path, report.seen);
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    report.errors.push((path.to_path_buf(), e.to_string()));
                    continue;
                }
            };
            let size = meta.len();
            let mtime_ns = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0);
            let key = path.to_string_lossy().into_owned();
            match known.remove(&key) {
                Some((s, m)) if s == size && m == mtime_ns => continue,
                Some(_) => report.updated += 1,
                None => report.added += 1,
            }
            let (info, error) = match probe(path) {
                Ok(i) => (Some(i), None),
                Err(e) => {
                    report.errors.push((path.to_path_buf(), format!("{e:#}")));
                    (None, Some(format!("{e:#}")))
                }
            };
            let dir = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            tx.execute(
                "INSERT INTO files (path, dir, name, size, mtime_ns, hash, duration, sample_rate, channels, bits, codec, container, lufs, true_peak, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, ?10, ?11, NULL, NULL, ?12)
                 ON CONFLICT(path) DO UPDATE SET dir=excluded.dir, name=excluded.name, size=excluded.size, mtime_ns=excluded.mtime_ns, hash=NULL,
                    duration=excluded.duration, sample_rate=excluded.sample_rate, channels=excluded.channels, bits=excluded.bits, codec=excluded.codec,
                    container=excluded.container, lufs=NULL, true_peak=NULL, error=excluded.error",
                params![
                    key,
                    dir,
                    name,
                    size as i64,
                    mtime_ns,
                    info.as_ref().map(|i| i.duration_secs),
                    info.as_ref().map(|i| i.sample_rate),
                    info.as_ref().map(|i| i.channels),
                    info.as_ref().and_then(|i| i.bits_per_sample),
                    info.as_ref().map(|i| i.codec.clone()),
                    info.as_ref().map(|i| i.container.clone()),
                    error,
                ],
            )?;
        }
        for gone in known.keys() {
            tx.execute("DELETE FROM files WHERE path = ?1", params![gone])?;
            report.removed += 1;
        }
        tx.commit()?;
        Ok(report)
    }

    pub fn forget_root(&self, root: &Path) -> Result<usize> {
        let prefix = format!("{}{}%", root.to_string_lossy(), std::path::MAIN_SEPARATOR);
        Ok(self.conn.execute("DELETE FROM files WHERE path LIKE ?1", params![prefix])?)
    }

    /// The file's content hash, computed and stored when the row has none yet.
    pub fn ensure_hash(&self, path: &Path) -> Result<String> {
        if let Some(h) = self.conn.query_row("SELECT hash FROM files WHERE path = ?1", params![path.to_string_lossy()], |r| r.get::<_, Option<String>>(0)).optional()?.flatten() {
            return Ok(h);
        }
        let hash = hash_file(path)?;
        self.conn.execute("UPDATE files SET hash = ?2 WHERE path = ?1", params![path.to_string_lossy(), hash])?;
        Ok(hash)
    }

    pub fn set_loudness(&self, path: &Path, lufs: f64, true_peak_dbfs: f64) -> Result<()> {
        self.conn.execute("UPDATE files SET lufs = ?2, true_peak = ?3 WHERE path = ?1", params![path.to_string_lossy(), lufs, true_peak_dbfs])?;
        Ok(())
    }

    pub fn set_tag(&self, hash: &str, tag: &str, on: bool) -> Result<()> {
        if on {
            self.conn.execute("INSERT OR IGNORE INTO file_tags (hash, tag) VALUES (?1, ?2)", params![hash, tag])?;
        } else {
            self.conn.execute("DELETE FROM file_tags WHERE hash = ?1 AND tag = ?2", params![hash, tag])?;
        }
        Ok(())
    }

    pub fn set_favorite(&self, hash: &str, on: bool) -> Result<()> {
        if on {
            self.conn.execute("INSERT OR IGNORE INTO favorites (project, hash) VALUES (?1, ?2)", params![self.project, hash])?;
        } else {
            self.conn.execute("DELETE FROM favorites WHERE project = ?1 AND hash = ?2", params![self.project, hash])?;
        }
        Ok(())
    }

    pub fn favorite_count(&self, project: &str) -> Result<usize> {
        Ok(self.conn.query_row("SELECT count(*) FROM favorites WHERE project = ?1", params![project], |r| r.get::<_, i64>(0))? as usize)
    }

    pub fn rename_project(&self, old: &str, new: &str) -> Result<usize> {
        Ok(self.conn.execute("UPDATE OR IGNORE favorites SET project = ?2 WHERE project = ?1", params![old, new])?)
    }

    pub fn forget_project(&self, project: &str) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM favorites WHERE project = ?1", params![project])?)
    }

    pub fn rename_tag(&self, old: &str, new: &str) -> Result<usize> {
        Ok(self.conn.execute("UPDATE OR IGNORE file_tags SET tag = ?2 WHERE tag = ?1", params![old, new])?)
    }

    pub fn remove_tag(&self, tag: &str) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM file_tags WHERE tag = ?1", params![tag])?)
    }

    pub fn tags_in_use(&self) -> Result<Vec<(String, usize)>> {
        let mut stmt = self.conn.prepare("SELECT tag, count(*) FROM file_tags GROUP BY tag ORDER BY tag")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}
