use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::log;

const LOCKFILE_PATH: &str = "tungsten.lock.toml";
const LOCKFILE_VERSION: u32 = 2;

/// Entries live in sorted maps so the file is always written in one stable
/// order: a sync that adds one asset adds one block, instead of reshuffling
/// the whole file and turning every merge into a conflict.
#[derive(Serialize, Deserialize, Default)]
pub struct Lockfile {
    pub version: u32,
    /// input_name -> cache key -> entry
    pub inputs: BTreeMap<String, BTreeMap<String, LockfileEntry>>,

    /// Whether any entry has been mutated since the last `save()`.
    /// Skipped during serialization — purely in-memory state.
    #[serde(skip)]
    dirty: bool,

    /// Pre-3.3 output-hash keys whose ids moved to a source key this run.
    /// Dropped on save, so the file keeps one entry per asset.
    #[serde(skip)]
    migrated: BTreeSet<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct LockfileEntry {
    /// Cloud asset ID (set after a successful `tungsten sync cloud` upload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<u64>,
    /// Studio content URI (set after a successful `tungsten sync studio` copy).
    /// Format: `rbxasset://.tungsten_<project>/<relative_path>`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio_uri: Option<String>,
}

#[allow(dead_code)]
impl Lockfile {
    pub fn load() -> Result<Self> {
        Self::load_from(Path::new(LOCKFILE_PATH))
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                version: LOCKFILE_VERSION,
                ..Self::default()
            });
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Could not read \"{}\"", path.display()))?;

        let mut lf = match toml::from_str::<Lockfile>(&content) {
            Ok(lf) => lf,
            Err(parse_error) => match salvage(&content) {
                Some(lf) => {
                    log!(
                        warn,
                        "\"{}\" had merge conflict markers or duplicate entries; kept the entries of both sides ({} total) and rewrote it",
                        path.display(),
                        lf.len()
                    );
                    lf
                }
                None => {
                    return Err(anyhow::Error::new(parse_error).context(
                        "Failed to parse lockfile — it may be corrupted, try deleting it and re-running",
                    ));
                }
            },
        };

        // v1 → v2: asset_id was a bare u64, now Option<u64>; serde(default)
        // coerces it on read. Any file that isn't exactly what save() would
        // write (an older version, pre-3.3 unsorted order, a salvaged merge)
        // is rewritten on the next save.
        lf.version = LOCKFILE_VERSION;
        let canonical = content.replace("\r\n", "\n");
        lf.dirty = lf.render().ok().as_deref() != Some(canonical.as_str());

        Ok(lf)
    }

    fn render(&self) -> Result<String> {
        toml::to_string(self).context("Failed to serialize lockfile")
    }

    /// Write to disk only if entries have changed since last save.
    pub fn save(&mut self) -> Result<()> {
        self.save_to(Path::new(LOCKFILE_PATH))
    }

    pub fn save_to(&mut self, path: &Path) -> Result<()> {
        for (input_name, hash) in std::mem::take(&mut self.migrated) {
            if let Some(entries) = self.inputs.get_mut(&input_name) {
                entries.remove(&hash);
            }
        }

        if !self.dirty {
            return Ok(());
        }

        let content = self.render()?;
        std::fs::write(path, content)
            .with_context(|| format!("Could not write \"{}\"", path.display()))?;

        self.dirty = false;
        Ok(())
    }

    /// Force a save regardless of dirty state. Useful for explicit flush points.
    pub fn force_save(&mut self) -> Result<()> {
        self.dirty = true;
        self.save()
    }

    /// Number of entries across all inputs.
    pub fn len(&self) -> usize {
        self.inputs.values().map(|entries| entries.len()).sum()
    }

    /// Look up a cached cloud asset ID.
    #[inline]
    pub fn get(&self, input_name: &str, hash: &str) -> Option<u64> {
        self.inputs.get(input_name)?.get(hash)?.asset_id
    }

    /// Look up a cached Studio content URI.
    #[inline]
    pub fn get_uri(&self, input_name: &str, hash: &str) -> Option<&str> {
        self.inputs
            .get(input_name)?
            .get(hash)?
            .studio_uri
            .as_deref()
    }

    /// Reuse the cloud asset recorded under a pre-3.3 output-hash key for its
    /// new source key. Older versions keyed images on their compressed bytes,
    /// which come out different on another machine; carrying the id over
    /// keeps the asset from re-uploading. The old entry is dropped on save.
    pub fn adopt_legacy(&mut self, input_name: &str, legacy_hash: &str, key: &str) -> Option<u64> {
        if legacy_hash == key {
            return None;
        }
        let entry = self.inputs.get(input_name)?.get(legacy_hash)?.clone();
        let id = entry.asset_id?;
        self.inputs
            .entry(input_name.to_string())
            .or_default()
            .insert(key.to_string(), entry);
        self.migrated
            .insert((input_name.to_string(), legacy_hash.to_string()));
        self.dirty = true;
        Some(id)
    }

    /// Store a cloud asset ID, preserving any existing studio_uri for the same hash.
    #[inline]
    pub fn set(&mut self, input_name: &str, hash: String, asset_id: u64) {
        let entry = self
            .inputs
            .entry(input_name.to_string())
            .or_default()
            .entry(hash)
            .or_default();
        entry.asset_id = Some(asset_id);
        self.dirty = true;
    }

    /// Store a Studio content URI, preserving any existing asset_id for the same hash.
    #[inline]
    pub fn set_uri(&mut self, input_name: &str, hash: String, uri: String) {
        let entry = self
            .inputs
            .entry(input_name.to_string())
            .or_default()
            .entry(hash)
            .or_default();
        entry.studio_uri = Some(uri);
        self.dirty = true;
    }

    /// Returns `true` if there are unsaved changes.
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

/// Recover a lockfile git merged with conflict markers, or a union merge that
/// left one entry twice. Every entry from every side is kept, since each one
/// is a real upload; a key that appears twice keeps its first value (both ids
/// point at uploads of the same content). Returns `None` when the text holds
/// no lockfile content at all.
fn salvage(content: &str) -> Option<Lockfile> {
    let mut lf = Lockfile {
        version: LOCKFILE_VERSION,
        ..Lockfile::default()
    };
    let mut current: Option<(String, String)> = None;
    let mut found = false;

    for line in content.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || is_conflict_marker(line) {
            continue;
        }

        if line.starts_with('[') {
            current = parse_entry_header(line);
            found |= current.is_some();
            continue;
        }

        let Ok(table) = toml::from_str::<toml::Table>(line) else {
            continue;
        };
        if table.contains_key("version") {
            found = true;
            continue;
        }
        let Some((input_name, hash)) = &current else {
            continue;
        };
        let entry = lf
            .inputs
            .entry(input_name.clone())
            .or_default()
            .entry(hash.clone())
            .or_default();
        if let Some(id) = table.get("asset_id").and_then(|v| v.as_integer()) {
            entry.asset_id.get_or_insert(id as u64);
        }
        if let Some(uri) = table.get("studio_uri").and_then(|v| v.as_str()) {
            entry.studio_uri.get_or_insert_with(|| uri.to_string());
        }
    }

    for entries in lf.inputs.values_mut() {
        entries.retain(|_, e| e.asset_id.is_some() || e.studio_uri.is_some());
    }
    lf.inputs.retain(|_, entries| !entries.is_empty());

    found.then_some(lf)
}

fn is_conflict_marker(line: &str) -> bool {
    ["<<<<<<<", "|||||||", "=======", ">>>>>>>"]
        .iter()
        .any(|marker| line.starts_with(marker))
}

/// `[inputs.<input>.<key>]` → `(input, key)`, quoted keys included.
fn parse_entry_header(line: &str) -> Option<(String, String)> {
    let table = toml::from_str::<toml::Table>(line).ok()?;
    let inputs = table.get("inputs")?.as_table()?;
    let (input_name, entries) = inputs.iter().next()?;
    let (hash, _) = entries.as_table()?.iter().next()?;
    Some((input_name.clone(), hash.clone()))
}

/// SHA-256 hex digest of raw bytes.
/// Used as the cache key in the lockfile.
pub fn hash_image(data: &[u8]) -> String {
    hash_parts(&[data])
}

/// SHA-256 hex digest of several byte slices hashed as one stream.
/// Used to build cache keys that mix content with processing settings.
pub fn hash_parts(parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_flag_on_set() {
        let mut lf = Lockfile::default();
        assert!(!lf.is_dirty());
        lf.set("icons", "abc123".to_string(), 999);
        assert!(lf.is_dirty());
    }

    #[test]
    fn test_get_after_set() {
        let mut lf = Lockfile::default();
        lf.set("icons", "hashval".to_string(), 42);
        assert_eq!(lf.get("icons", "hashval"), Some(42));
        assert_eq!(lf.get("icons", "nope"), None);
        assert_eq!(lf.get("other", "hashval"), None);
    }

    #[test]
    fn test_save_no_write_when_clean() {
        // save() on an unmodified lockfile should be a no-op (no I/O).
        // We can't easily assert no I/O, but we can at least assert it returns Ok.
        let mut lf = Lockfile::default();
        assert!(lf.save().is_ok());
    }

    fn written(lf: &mut Lockfile) -> String {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tungsten.lock.toml");
        lf.force_save_to(&path);
        std::fs::read_to_string(path).unwrap()
    }

    impl Lockfile {
        fn force_save_to(&mut self, path: &Path) {
            self.dirty = true;
            self.save_to(path).unwrap();
        }
    }

    #[test]
    fn test_save_order_is_independent_of_insert_order() {
        let mut a = Lockfile::default();
        let mut b = Lockfile::default();
        let entries = [("sounds", "ff", 1), ("icons", "0a", 2), ("icons", "b7", 3)];
        for (input, hash, id) in entries {
            a.set(input, hash.to_string(), id);
        }
        for (input, hash, id) in entries.iter().rev() {
            b.set(input, hash.to_string(), *id);
        }
        let text = written(&mut a);
        assert_eq!(text, written(&mut b));
        let icons_a = text.find("[inputs.icons.0a]").unwrap();
        let icons_b = text.find("[inputs.icons.b7]").unwrap();
        let sounds = text.find("[inputs.sounds.ff]").unwrap();
        assert!(icons_a < icons_b && icons_b < sounds, "{}", text);
    }

    #[test]
    fn test_canonical_file_loads_clean_and_unsorted_file_loads_dirty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tungsten.lock.toml");

        let mut lf = Lockfile {
            version: LOCKFILE_VERSION,
            ..Lockfile::default()
        };
        lf.set("icons", "aa".to_string(), 1);
        lf.set("icons", "bb".to_string(), 2);
        lf.save_to(&path).unwrap();
        assert!(!Lockfile::load_from(&path).unwrap().is_dirty());

        std::fs::write(
            &path,
            "version = 2\n\n[inputs.icons.bb]\nasset_id = 2\n\n[inputs.icons.aa]\nasset_id = 1\n",
        )
        .unwrap();
        let reloaded = Lockfile::load_from(&path).unwrap();
        assert!(reloaded.is_dirty());
        assert_eq!(reloaded.get("icons", "aa"), Some(1));
    }

    #[test]
    fn test_conflicted_lockfile_keeps_both_sides() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tungsten.lock.toml");
        std::fs::write(
            &path,
            "version = 2\n\n\
             [inputs.icons.aa]\nasset_id = 1\n\n\
             <<<<<<< HEAD\n\
             [inputs.icons.bb]\nasset_id = 2\n\
             ||||||| base\n\
             =======\n\
             [inputs.\"Other Input\".cc]\nasset_id = 3\n\n\
             [inputs.icons.bb]\nasset_id = 9\n\
             >>>>>>> theirs\n\n\
             [inputs.icons.dd]\nstudio_uri = \"rbxasset://x.png\"\n",
        )
        .unwrap();

        let lf = Lockfile::load_from(&path).unwrap();
        assert!(lf.is_dirty());
        assert_eq!(lf.get("icons", "aa"), Some(1));
        assert_eq!(
            lf.get("icons", "bb"),
            Some(2),
            "first side wins a duplicate key"
        );
        assert_eq!(lf.get("Other Input", "cc"), Some(3));
        assert_eq!(lf.get_uri("icons", "dd"), Some("rbxasset://x.png"));
        assert_eq!(lf.len(), 4);
    }

    #[test]
    fn test_garbage_lockfile_still_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tungsten.lock.toml");
        std::fs::write(&path, "this is = = not a lockfile").unwrap();
        assert!(Lockfile::load_from(&path).is_err());
    }

    #[test]
    fn test_adopt_legacy_moves_id_to_source_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tungsten.lock.toml");

        let mut lf = Lockfile::default();
        lf.set("icons", "legacy".to_string(), 77);
        assert_eq!(lf.adopt_legacy("icons", "legacy", "source"), Some(77));
        assert_eq!(lf.adopt_legacy("icons", "missing", "other"), None);
        // A second asset with the same processed bytes can still adopt it
        // within the same run; the legacy entry only goes away on save.
        assert_eq!(lf.adopt_legacy("icons", "legacy", "source2"), Some(77));
        lf.save_to(&path).unwrap();

        assert_eq!(lf.get("icons", "source"), Some(77));
        assert_eq!(lf.get("icons", "source2"), Some(77));
        assert_eq!(lf.get("icons", "legacy"), None);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("legacy"));
    }
}
