//! The library database: every audio file under the roots with its probed metadata, and the
//! marks (tags, favorites) keyed by content hash so they follow a file through renames.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::config::ExportPreset;
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

/// A column a listing is ordered by. Files missing the value sort last either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Name,
    Folder,
    Length,
    Channels,
    Rate,
    Bits,
    Codec,
    Loudness,
    Size,
}

impl SortKey {
    pub const ALL: [SortKey; 9] = [SortKey::Name, SortKey::Folder, SortKey::Length, SortKey::Channels, SortKey::Rate, SortKey::Bits, SortKey::Codec, SortKey::Loudness, SortKey::Size];

    pub fn name(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Folder => "folder",
            SortKey::Length => "length",
            SortKey::Channels => "channels",
            SortKey::Rate => "rate",
            SortKey::Bits => "bits",
            SortKey::Codec => "codec",
            SortKey::Loudness => "loudness",
            SortKey::Size => "size",
        }
    }

    fn column(self) -> &'static str {
        match self {
            SortKey::Name => "f.name COLLATE NOCASE",
            SortKey::Folder => "f.dir COLLATE NOCASE",
            SortKey::Length => "f.duration",
            SortKey::Channels => "f.channels",
            SortKey::Rate => "f.sample_rate",
            SortKey::Bits => "f.bits",
            SortKey::Codec => "f.codec",
            SortKey::Loudness => "f.lufs",
            SortKey::Size => "f.size",
        }
    }
}

impl std::str::FromStr for SortKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        SortKey::ALL.into_iter().find(|k| k.name() == s).ok_or_else(|| format!("{s}: expected one of {}", SortKey::ALL.map(SortKey::name).join(", ")))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub key: SortKey,
    #[serde(default)]
    pub descending: bool,
}

impl Sort {
    /// Ties and files without the value fall back to the name, so the order is stable.
    fn clause(self) -> String {
        let dir = if self.descending { "DESC" } else { "ASC" };
        let col = self.key.column();
        match self.key {
            SortKey::Name => format!(" ORDER BY {col} {dir}, f.path"),
            _ => format!(" ORDER BY {col} IS NULL, {col} {dir}, f.name COLLATE NOCASE"),
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
    /// Folders to search within; `None` is the whole library, and an empty list finds nothing.
    pub scope: Option<Vec<PathBuf>>,
    pub favorites_only: bool,
    /// Files carrying any of these tags.
    pub tags: Vec<String>,
    /// Lowercase extensions without the dot.
    pub extensions: Vec<String>,
    pub channels: Vec<ChannelClass>,
    pub sort: Sort,
    pub limit: usize,
}

/// Where a project's export card and Preview were left, kept per project like favorites so the
/// project file does not change with them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CardState {
    /// The preset the card was on; `None` once its values were edited.
    pub preset_name: Option<String>,
    /// The card's values, which stand when the named preset is gone or there is none.
    pub preset: ExportPreset,
    pub preview: bool,
    pub overwrite: bool,
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
CREATE TABLE IF NOT EXISTS hashes (hash TEXT PRIMARY KEY, size INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS card_state (project TEXT PRIMARY KEY, state TEXT NOT NULL);
INSERT OR IGNORE INTO hashes SELECT hash, size FROM files WHERE hash IS NOT NULL;
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
    pub fn list_dir(&self, dir: &Path, sort: Sort) -> Result<Vec<FileRecord>> {
        self.search(&Query { dir: Some(dir.to_path_buf()), sort, ..Query::default() })
    }

    pub fn search(&self, q: &Query) -> Result<Vec<FileRecord>> {
        let (filter, names, values) = Self::filter(q);
        let mut sql = format!("{SELECT}{filter}");
        sql.push_str(&q.sort.clause());
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

    /// How many files `search` would return, without its limit.
    pub fn count(&self, q: &Query) -> Result<usize> {
        let (filter, names, values) = Self::filter(q);
        let sql = format!("SELECT count(*) FROM files f{filter}");
        let mut params: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        if q.favorites_only {
            params.push((":project", &self.project));
        }
        for (n, v) in names.iter().zip(values.iter()) {
            params.push((n.as_str(), v));
        }
        Ok(self.conn.query_row(&sql, params.as_slice(), |r| r.get::<_, i64>(0))? as usize)
    }

    /// The WHERE clause for `q`, with its parameter names and values.
    fn filter(q: &Query) -> (String, Vec<String>, Vec<String>) {
        let mut sql = String::from(" WHERE 1=1");
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
        let within = |names: &mut Vec<String>, values: &mut Vec<String>, dir: &Path| -> String {
            let (lo, hi) = below(dir);
            let lo = bind(names, values, lo);
            let hi = bind(names, values, hi);
            format!("(f.path >= {lo} AND f.path < {hi})")
        };
        if let Some(under) = &q.under {
            sql.push_str(&format!(" AND {}", within(&mut names, &mut values, under)));
        }
        match &q.scope {
            Some(scope) if scope.is_empty() => sql.push_str(" AND 0"),
            Some(scope) => {
                let ors: Vec<String> = scope.iter().map(|s| within(&mut names, &mut values, s)).collect();
                sql.push_str(&format!(" AND ({})", ors.join(" OR ")));
            }
            None => {}
        }
        (sql, names, values)
    }

    pub fn count_dir(&self, dir: &Path) -> Result<usize> {
        Ok(self.conn.query_row("SELECT count(*) FROM files WHERE dir = ?1", params![dir.to_string_lossy()], |r| r.get::<_, i64>(0))? as usize)
    }

    /// Walks `root`, probes new and changed files, and drops rows whose file is gone. A root
    /// that cannot be reached is an error, not an empty folder: its rows stay as they are.
    pub fn scan(&mut self, root: &Path, mut progress: impl FnMut(&Path, usize)) -> Result<ScanReport> {
        if !root.is_dir() {
            anyhow::bail!("{} is not reachable; its files are kept as last seen", root.display());
        }
        let mut report = ScanReport::default();
        let (lo, hi) = below(root);
        let mut known: std::collections::HashMap<String, (u64, i64)> = {
            let mut stmt = self.conn.prepare("SELECT path, size, mtime_ns FROM files WHERE path >= ?1 AND path < ?2")?;
            let rows = stmt.query_map(params![lo, hi], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)?))))?;
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

    /// Hashes unhashed files until every tag and favorite whose file is unknown has found it
    /// again, so a moved or renamed file carries its marks by content. Only files the size of
    /// a missing one are read; a mark from before sizes were recorded has no size and admits
    /// every unhashed file. Returns how many files were hashed; with no such marks that is
    /// zero and nothing is read.
    pub fn match_orphaned_marks(&self, mut progress: impl FnMut(&Path, usize)) -> Result<usize> {
        let orphans: Vec<(String, Option<i64>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT o.hash, h.size FROM (SELECT hash FROM file_tags UNION SELECT hash FROM favorites EXCEPT SELECT hash FROM files WHERE hash IS NOT NULL) o
                 LEFT JOIN hashes h ON h.hash = o.hash",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if orphans.is_empty() {
            return Ok(0);
        }
        let any_size = orphans.iter().any(|(_, size)| size.is_none());
        let sizes: std::collections::HashSet<i64> = orphans.iter().filter_map(|(_, size)| *size).collect();
        let mut orphans: std::collections::HashSet<String> = orphans.into_iter().map(|(hash, _)| hash).collect();
        let unhashed: Vec<(String, i64)> = {
            let mut stmt = self.conn.prepare("SELECT path, size FROM files WHERE hash IS NULL AND error IS NULL ORDER BY path")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut hashed = 0;
        for (path, size) in &unhashed {
            if orphans.is_empty() {
                break;
            }
            if !any_size && !sizes.contains(size) {
                continue;
            }
            let path = Path::new(path);
            let Ok(hash) = hash_file(path) else { continue };
            self.record_hash(path, &hash)?;
            orphans.remove(&hash);
            hashed += 1;
            progress(path, hashed);
        }
        Ok(hashed)
    }

    pub fn forget_root(&self, root: &Path) -> Result<usize> {
        let (lo, hi) = below(root);
        Ok(self.conn.execute("DELETE FROM files WHERE path >= ?1 AND path < ?2", params![lo, hi])?)
    }

    /// The file's content hash, computed and stored when the row has none yet.
    pub fn ensure_hash(&self, path: &Path) -> Result<String> {
        if let Some(h) = self.conn.query_row("SELECT hash FROM files WHERE path = ?1", params![path.to_string_lossy()], |r| r.get::<_, Option<String>>(0)).optional()?.flatten() {
            return Ok(h);
        }
        let hash = hash_file(path)?;
        self.record_hash(path, &hash)?;
        Ok(hash)
    }

    /// Stores a computed hash on its row, and the row's size against the hash so a mark whose
    /// file goes missing still knows how big that file was.
    fn record_hash(&self, path: &Path, hash: &str) -> Result<()> {
        let path = path.to_string_lossy();
        self.conn.execute("UPDATE files SET hash = ?2 WHERE path = ?1", params![path, hash])?;
        self.conn.execute("INSERT OR IGNORE INTO hashes SELECT hash, size FROM files WHERE path = ?1", params![path])?;
        Ok(())
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

    pub fn rename_project(&self, old: &str, new: &str) -> Result<usize> {
        Ok(self.conn.execute("UPDATE OR IGNORE favorites SET project = ?2 WHERE project = ?1", params![old, new])?)
    }

    pub fn forget_project(&self, project: &str) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM favorites WHERE project = ?1", params![project])?)
    }

    /// The project's card as it was left; `None` for a project never opened in the window.
    pub fn card_state(&self) -> Result<Option<CardState>> {
        let json: Option<String> = self.conn.query_row("SELECT state FROM card_state WHERE project = ?1", params![self.project], |r| r.get(0)).optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }

    pub fn set_card_state(&self, state: &CardState) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO card_state (project, state) VALUES (?1, ?2)", params![self.project, serde_json::to_string(state)?])?;
        Ok(())
    }

    /// Cards of every project that were on preset `old` follow its rename.
    pub fn rename_card_preset(&self, old: &str, new: &str) -> Result<usize> {
        Ok(self.conn.execute("UPDATE card_state SET state = json_set(state, '$.preset_name', ?2) WHERE json_extract(state, '$.preset_name') = ?1", params![old, new])?)
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

/// The half-open range of path strings under `dir`: a seek on the path index, where a LIKE
/// prefix scans it and reads `_` and `%` in a folder name as wildcards.
fn below(dir: &Path) -> (String, String) {
    let lo = format!("{}{}", dir.to_string_lossy(), std::path::MAIN_SEPARATOR);
    let hi = format!("{}{}", dir.to_string_lossy(), (std::path::MAIN_SEPARATOR as u8 + 1) as char);
    (lo, hi)
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder for one test, with a readable WAV per name whose content follows the name
    /// and whose data is the given number of bytes.
    fn library(test: &str, files: &[(&str, usize)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hark-library-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, bytes_of_data) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let samples: Vec<u8> = name.bytes().cycle().take(*bytes_of_data).collect();
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16u32.to_le_bytes());
            for v in [1u16, 1] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            bytes.extend_from_slice(&48_000u32.to_le_bytes());
            bytes.extend_from_slice(&96_000u32.to_le_bytes());
            for v in [2u16, 16] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&(samples.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&samples);
            std::fs::write(path, bytes).unwrap();
        }
        root
    }

    #[test]
    fn marks_follow_a_moved_file() {
        let root = library("moved", &[("a/one.wav", 400), ("a/two.wav", 400), ("a/three.wav", 600)]);
        let mut db = Db::open_memory().unwrap();
        db.scan(&root, |_, _| {}).unwrap();
        let hash = db.ensure_hash(&root.join("a/one.wav")).unwrap();
        db.set_tag(&hash, "hit", true).unwrap();
        db.set_favorite(&hash, true).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::rename(root.join("a/one.wav"), root.join("b/moved.wav")).unwrap();
        db.scan(&root, |_, _| {}).unwrap();
        let mut read = Vec::new();
        db.match_orphaned_marks(|p, _| read.push(p.to_path_buf())).unwrap();
        let moved = db.get(&root.join("b/moved.wav")).unwrap().unwrap();
        assert_eq!(moved.tags, vec!["hit"]);
        assert!(moved.favorite);
        assert!(!read.contains(&root.join("a/three.wav")), "a file of another size is never read");
        assert_eq!(db.match_orphaned_marks(|_, _| {}).unwrap(), 0, "nothing left to find");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unreachable_root_keeps_its_rows() {
        let root = library("unplugged", &[("one.wav", 400), ("two.wav", 400)]);
        let mut db = Db::open_memory().unwrap();
        db.scan(&root, |_, _| {}).unwrap();
        let parked = root.with_extension("parked");
        std::fs::rename(&root, &parked).unwrap();
        assert!(db.scan(&root, |_, _| {}).is_err());
        assert_eq!(db.search(&Query { under: Some(root.clone()), ..Query::default() }).unwrap().len(), 2);
        std::fs::remove_dir_all(&parked).unwrap();
    }
}
