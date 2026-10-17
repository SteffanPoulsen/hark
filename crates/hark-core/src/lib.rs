pub mod audition;
pub mod config;
pub mod decode;
pub mod export;
pub mod flac;
pub mod keys;
pub mod library;
pub mod loudness;
pub mod origin;
pub mod paths;
pub mod pcm;
pub mod peaks;
pub mod project;
pub mod resample;
pub mod copies;
pub mod update;
pub mod wav;

pub use config::{Config, ExportPreset, Root, TagDef};
pub use project::Project;
pub use decode::{decode, probe, AudioInfo};
pub use export::{ExportRequest, ExportResult, Selection};
pub use library::{Db, FileRecord};
pub use loudness::Loudness;
pub use origin::Origin;
pub use pcm::Pcm;
pub use peaks::Peaks;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
