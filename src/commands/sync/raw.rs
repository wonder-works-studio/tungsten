use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;
use relative_path::RelativePathBuf;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::api::sync::debug::DebugSync;
use crate::api::sync::roblox::Creator;
use crate::api::sync::studio::StudioSync;
use crate::api::upload::{RobloxClient, UploadParams};
use crate::core::assets::asset::{
    self, AssetKind, AssetMeta, AudioFormat, WebAsset, is_animation_file,
};
use crate::core::assets::audio::transcode::{ResolvedAudioOptions, transcode_to_ogg};
use crate::core::assets::img::compress::CompressOptions;
use crate::core::assets::img::convert;
use crate::core::postsync::codegen::{self, CodegenEntry};
use crate::core::postsync::lockfile::{Lockfile, hash_image, hash_parts};
use crate::log;
use crate::utils::logger::{clear_progress_line, progress};

use super::Target;
use super::codegen_write::{seed_web_assets, write_codegen};
use super::paths::relative_path;

pub struct RawPending {
    pub name: String,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub hash: String,
    pub kind: AssetKind,
    /// Output file extension. Matches the source extension unless the asset
    /// was transcoded (e.g. "wav" becomes "ogg").
    pub ext: String,
    pub display_name: String,
    pub description: String,
}

/// Optionally compress `bytes` using the provided options.
/// Returns the (possibly compressed) bytes.
#[inline]
fn maybe_compress(
    bytes: Vec<u8>,
    ext: &str,
    compress_options: Option<&CompressOptions>,
) -> Vec<u8> {
    let Some(opts) = compress_options else {
        return bytes;
    };
    match convert::normalize_for_compression(bytes.clone(), ext) {
        Ok((normalized, norm_ext)) => {
            match crate::core::assets::img::compress::compress_image(&normalized, norm_ext, opts) {
                Ok(compressed) => compressed,
                Err(e) => {
                    clear_progress_line();
                    log!(warn, "Compression failed, using original: {}", e);
                    bytes
                }
            }
        }
        Err(e) => {
            clear_progress_line();
            log!(warn, "Could not normalize for compression: {}", e);
            bytes
        }
    }
}

/// A file that has been read and classified, but not yet processed.
struct PreparedFile {
    path: PathBuf,
    bytes: Vec<u8>,
    kind: AssetKind,
    src_ext: String,
    name: String,
    /// Source-based lockfile key for audio inputs with processing enabled.
    /// Keyed on the source bytes and settings — not the encoded output —
    /// so unchanged files can skip the decode/encode round-trip entirely
    /// and non-deterministic encoder output can't invalidate the cache.
    audio_cache_key: Option<String>,
}

/// Read and classify a file without doing any expensive processing.
fn prepare_raw_file(
    path: &PathBuf,
    base_path: &str,
    audio_options: Option<&ResolvedAudioOptions>,
) -> Result<PreparedFile, ProcessingError> {
    let bytes = std::fs::read(path).map_err(|e| ProcessingError {
        name: path.display().to_string(),
        error: anyhow::anyhow!("Failed to read \"{}\": {}", path.display(), e),
    })?;

    let src_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let kind = match asset::kind_from_ext(&src_ext) {
        Some(k) => {
            // Check if .rbxm/.rbxmx is actually an animation
            if (src_ext == "rbxm" || src_ext == "rbxmx")
                && k == AssetKind::Model(asset::ModelFormat::Roblox)
            {
                if is_animation_file(path).unwrap_or(false) {
                    AssetKind::Animation
                } else {
                    k
                }
            } else {
                k
            }
        }
        None => {
            return Err(ProcessingError {
                name: path.display().to_string(),
                error: anyhow::anyhow!("Unsupported extension \"{}\"", src_ext),
            });
        }
    };

    let name = {
        let rel = relative_path(path, base_path);
        Path::new(&rel)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/")
    };

    let audio_cache_key = match (kind, audio_options) {
        (AssetKind::Audio(_), Some(opts)) => {
            Some(hash_parts(&[&bytes, opts.fingerprint().as_bytes()]))
        }
        _ => None,
    };

    Ok(PreparedFile {
        path: path.clone(),
        bytes,
        kind,
        src_ext,
        name,
        audio_cache_key,
    })
}

/// Finish processing a prepared file: transcode, compress and hash.
fn finish_raw_file(
    prep: PreparedFile,
    compress_options: Option<&CompressOptions>,
    audio_options: Option<&ResolvedAudioOptions>,
    dry_run: bool,
) -> Result<RawPending, ProcessingError> {
    let PreparedFile {
        path,
        bytes,
        kind,
        src_ext,
        name,
        audio_cache_key,
    } = prep;

    // Transcode audio to OGG Vorbis when the input opts in.
    let (data, kind, out_ext) = if let (AssetKind::Audio(_), Some(opts)) = (kind, audio_options) {
        if dry_run {
            // Dry runs never upload or copy the bytes — skip the expensive
            // decode/encode round-trip.
            (Vec::new(), kind, src_ext.clone())
        } else {
            let started = std::time::Instant::now();
            match transcode_to_ogg(&bytes, &src_ext, opts) {
                Ok(Some(ogg)) => {
                    log!(
                        debug,
                        "Transcoded \"{}\" in {:.2}s ({} KB -> {} KB)",
                        name,
                        started.elapsed().as_secs_f64(),
                        bytes.len() / 1024,
                        ogg.len() / 1024
                    );
                    (ogg, AssetKind::Audio(AudioFormat::Ogg), "ogg".to_string())
                }
                // Already OGG — passed through untouched.
                Ok(None) => (bytes, kind, src_ext.clone()),
                Err(e) => {
                    return Err(ProcessingError {
                        name: path.display().to_string(),
                        error: e.context(format!(
                            "Failed to transcode \"{}\" to OGG Vorbis",
                            path.display()
                        )),
                    });
                }
            }
        }
    } else {
        if let AssetKind::Audio(format) = kind
            && !format.roblox_supported()
        {
            clear_progress_line();
            log!(
                warn,
                "\"{}\" is a format Roblox does not accept — add an [inputs.<name>.audio] section to transcode it to OGG",
                path.display()
            );
        }
        (bytes, kind, src_ext.clone())
    };

    let data = if data.is_empty() {
        data
    } else {
        maybe_compress(data, &out_ext, compress_options)
    };
    // Processed audio is keyed by source bytes and settings; everything else
    // by its final bytes.
    let hash = match audio_cache_key {
        Some(key) => key,
        None => hash_image(&data),
    };
    let meta = AssetMeta::load_for(&path).unwrap_or_default();
    let display_name = meta.resolve_name(&name).to_string();
    let description = meta.resolve_description("Uploaded by Tungsten").to_string();

    Ok(RawPending {
        name,
        path,
        bytes: data,
        hash,
        kind,
        ext: out_ext,
        display_name,
        description,
    })
}

/// Error type for asset processing failures
#[derive(Debug)]
struct ProcessingError {
    name: String,
    error: anyhow::Error,
}

/// Split prepared assets into those still needing an upload and those already
/// recorded in the lockfile, returned as `(name, cached_asset_id)` pairs.
///
/// Cache hits are pulled out before the progress bar is sized so the counter
/// reflects real work: adding one sound to a folder of thirty reports `1/1`,
/// not `31/31`.
fn split_cached<'a>(
    pending: &'a [RawPending],
    input_name: &str,
    lockfile: &Lockfile,
    use_cache: bool,
) -> (Vec<&'a RawPending>, Vec<(String, u64)>) {
    let mut work: Vec<&RawPending> = Vec::with_capacity(pending.len());
    let mut cached: Vec<(String, u64)> = Vec::new();

    for p in pending {
        match use_cache
            .then(|| lockfile.get(input_name, &p.hash))
            .flatten()
        {
            Some(id) => cached.push((p.name.clone(), id)),
            None => work.push(p),
        }
    }

    (work, cached)
}

#[allow(clippy::too_many_arguments)]
pub async fn process_raw(
    input_name: &str,
    paths: Vec<PathBuf>,
    base_path: &str,
    output_path: &str,
    codegen_style: &str,
    strip_extension: bool,
    ts_declaration: bool,
    compress_options: Option<&CompressOptions>,
    audio_options: Option<&ResolvedAudioOptions>,
    target: Target,
    dry_run: bool,
    creator: &Creator,
    asset_type: Option<&str>,
    client: &Option<Arc<RobloxClient>>,
    studio_sync: &Option<Arc<StudioSync>>,
    debug_sync: &Option<Arc<DebugSync>>,
    lockfile: &mut Lockfile,
    studio_expected_files: &mut Option<&mut HashSet<String>>,
    max_concurrent_uploads: usize,
    web_assets: &HashMap<RelativePathBuf, WebAsset>,
) -> u32 {
    // Seed web assets into codegen entries first
    let mut codegen_entries: Vec<CodegenEntry> = Vec::new();
    seed_web_assets(web_assets, base_path, strip_extension, &mut codegen_entries);

    // Phase 1: read and classify files in parallel (no expensive work yet).
    let prepared_results: Vec<Result<PreparedFile, ProcessingError>> = paths
        .into_par_iter()
        .map(|path| prepare_raw_file(&path, base_path, audio_options))
        .collect::<Vec<_>>();

    let mut errors = 0u32;
    let mut prepared: Vec<PreparedFile> = Vec::new();
    for result in prepared_results {
        match result {
            Ok(p) => prepared.push(p),
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to prepare file \"{}\": {}", e.name, e.error);
                errors += 1;
            }
        }
    }

    // Phase 2: serve already-uploaded audio straight from the lockfile so
    // unchanged files skip the decode/encode round-trip entirely.
    let mut pending: Vec<RawPending> = Vec::new();
    let mut to_process: Vec<PreparedFile> = Vec::new();
    for prep in prepared {
        let cached = prep
            .audio_cache_key
            .as_ref()
            .filter(|_| target == Target::Cloud && !dry_run)
            .and_then(|key| lockfile.get(input_name, key).map(|id| (key.clone(), id)));
        match cached {
            // The lockfile hit is reported once, later, by split_cached.
            Some((key, _id)) => {
                let meta = AssetMeta::load_for(&prep.path).unwrap_or_default();
                pending.push(RawPending {
                    display_name: meta.resolve_name(&prep.name).to_string(),
                    description: meta.resolve_description("Uploaded by Tungsten").to_string(),
                    name: prep.name,
                    path: prep.path,
                    // Never uploaded or copied — the cached ID is reused.
                    bytes: Vec::new(),
                    hash: key,
                    kind: prep.kind,
                    ext: prep.src_ext,
                });
            }
            None => to_process.push(prep),
        }
    }

    // Phase 3: transcode, compress and hash the remainder in parallel.
    let finished_results: Vec<Result<RawPending, ProcessingError>> = to_process
        .into_par_iter()
        .map(|prep| finish_raw_file(prep, compress_options, audio_options, dry_run))
        .collect::<Vec<_>>();
    for result in finished_results {
        match result {
            Ok(p) => pending.push(p),
            Err(e) => {
                clear_progress_line();
                log!(warn, "Failed to prepare file \"{}\": {}", e.name, e.error);
                errors += 1;
            }
        }
    }

    // Serve lockfile hits before sizing the progress bar so it counts only
    // assets that actually get uploaded this run.
    let (work, cached) = split_cached(
        &pending,
        input_name,
        lockfile,
        target == Target::Cloud && !dry_run,
    );

    for (name, cached_id) in &cached {
        log!(
            debug,
            "{}: unchanged, skipping (cached asset {})",
            name,
            cached_id
        );
        codegen_entries.push(CodegenEntry::asset_id(name.clone(), *cached_id));
    }
    if !cached.is_empty() {
        log!(info, "{} file(s) unchanged, skipping upload", cached.len());
    }

    let total = work.len();

    // Configure upload concurrency limit from config
    let semaphore = Arc::new(Semaphore::new(max_concurrent_uploads));

    let mut upload_tasks: JoinSet<Result<(String, u64, String)>> = JoinSet::new();
    let mut dispatched = 0usize;

    for p in work {
        if dry_run {
            dispatched += 1;
            progress("Uploading", dispatched, total, p.name.as_str());
            codegen_entries.push(CodegenEntry::asset_id(p.name.clone(), 0));
            continue;
        }

        match target {
            Target::Studio => {
                // Models and Animations cannot be synced to Studio without prior upload
                if matches!(p.kind, AssetKind::Model(_) | AssetKind::Animation) {
                    dispatched += 1;
                    let uri = match lockfile.get(input_name, &p.hash) {
                        Some(cached_id) => format!("rbxassetid://{cached_id}"),
                        None => {
                            clear_progress_line();
                            log!(
                                warn,
                                "Models and Animations cannot be synced to Studio without having been uploaded first: \"{}\"",
                                p.name
                            );
                            errors += 1;
                            continue;
                        }
                    };
                    lockfile.set_uri(input_name, p.hash.clone(), uri.clone());
                    progress("Copying", dispatched, total, p.name.as_str());
                    codegen_entries.push(CodegenEntry::asset(
                        p.name.clone(),
                        codegen::AssetRef::Uri(uri),
                    ));
                } else {
                    dispatched += 1;
                    let rel = if p.ext.is_empty() {
                        p.name.clone()
                    } else {
                        format!("{}.{}", p.name, p.ext)
                    };
                    let uri = if let Some(ss) = studio_sync {
                        match ss.copy_asset(&rel, &p.bytes) {
                            Ok(u) => {
                                // Track expected file for Studio sync cleanup
                                if let Some(ref mut set) = *studio_expected_files {
                                    set.insert(rel.clone());
                                }
                                u
                            }
                            Err(e) => {
                                clear_progress_line();
                                log!(warn, "Studio copy failed for \"{}\": {}", p.name, e);
                                errors += 1;
                                continue;
                            }
                        }
                    } else {
                        String::new()
                    };
                    lockfile.set_uri(input_name, p.hash.clone(), uri.clone());
                    progress("Copying", dispatched, total, p.name.as_str());
                    codegen_entries.push(CodegenEntry::asset(
                        p.name.clone(),
                        codegen::AssetRef::Uri(uri),
                    ));
                }
            }
            Target::Debug => {
                dispatched += 1;
                let rel = format!(
                    "{}.{}",
                    p.name,
                    if p.ext.is_empty() { "bin" } else { &p.ext }
                );
                if let Some(ds) = debug_sync
                    && let Err(e) = ds.copy_asset(&rel, &p.bytes)
                {
                    clear_progress_line();
                    log!(warn, "Debug copy failed for \"{}\": {}", p.name, e);
                    errors += 1;
                    continue;
                }
                let fallback_id = lockfile.get(input_name, &p.hash).unwrap_or(0);
                progress("Copying", dispatched, total, p.name.as_str());
                codegen_entries.push(CodegenEntry::asset_id(p.name.clone(), fallback_id));
            }
            Target::Cloud => {
                // Lockfile hits were already resolved by split_cached.
                let Some(c) = client else {
                    codegen_entries.push(CodegenEntry::asset_id(p.name.clone(), 0));
                    continue;
                };
                let c_arc = Arc::clone(c);
                let creator_own = creator.clone();
                let p_path = p.path.clone();
                let p_name = p.name.clone();
                let p_hash = p.hash.clone();
                let p_ext = p.ext.clone();
                let p_display_name = p.display_name.clone();
                let p_description = p.description.clone();
                let p_bytes = p.bytes.clone();
                let p_kind = p.kind;
                let asset_type_override = asset_type.map(|s| s.to_string());
                let semaphore_clone = semaphore.clone();
                upload_tasks.spawn(async move {
                    let _permit = semaphore_clone.acquire_owned().await;
                    // Build the upload file name from the output extension,
                    // which may differ from the source after transcoding.
                    let stem = p_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let file_name = if p_ext.is_empty() {
                        stem
                    } else {
                        format!("{}.{}", stem, p_ext)
                    };
                    let id = c_arc
                        .upload(UploadParams {
                            file_name,
                            display_name: p_display_name.clone(),
                            description: p_description.clone(),
                            data: p_bytes.clone(),
                            kind: p_kind,
                            asset_type_override: asset_type_override.clone(),
                            creator: creator_own,
                        })
                        .await
                        .with_context(|| format!("Failed to upload \"{}\"", p_name.clone()))?;
                    Ok((p_name.clone(), id, p_hash.clone()))
                });
            }
        }
    }

    let mut completed = 0usize;
    while let Some(res) = upload_tasks.join_next().await {
        completed += 1;
        match res {
            Ok(Ok((name, id, hash))) => {
                lockfile.set(input_name, hash, id);
                progress("Uploading", dispatched + completed, total, &name);
                codegen_entries.push(CodegenEntry::asset_id(name.to_string(), id));
            }
            Ok(Err(e)) => {
                clear_progress_line();
                log!(warn, "{}", e);
                errors += 1;
            }
            Err(e) => {
                clear_progress_line();
                log!(warn, "Upload task panicked: {}", e);
                errors += 1;
            }
        }
    }

    write_codegen(
        codegen_entries,
        input_name,
        output_path,
        codegen_style,
        strip_extension,
        ts_declaration,
        &mut errors,
    );
    errors
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::sync::roblox::UserCreator;
    use crate::core::assets::audio::encode::VorbisProfile;
    use crate::core::assets::audio::transcode::tests::{make_wav, sine_samples};

    fn raw_pending(name: &str, hash: &str) -> RawPending {
        RawPending {
            name: name.to_string(),
            path: PathBuf::from(format!("{}.wav", name)),
            bytes: Vec::new(),
            hash: hash.to_string(),
            kind: AssetKind::Audio(AudioFormat::Wav),
            ext: "wav".to_string(),
            display_name: name.to_string(),
            description: String::new(),
        }
    }

    #[test]
    fn test_split_cached_counts_only_new_uploads() {
        // Two sounds already uploaded, one just added: the progress bar should
        // be sized 1, not 3.
        let pending = vec![
            raw_pending("old_one", "hash_a"),
            raw_pending("old_two", "hash_b"),
            raw_pending("brand_new", "hash_c"),
        ];
        let mut lockfile = Lockfile::default();
        lockfile.set("sounds", "hash_a".to_string(), 111);
        lockfile.set("sounds", "hash_b".to_string(), 222);

        let (work, cached) = split_cached(&pending, "sounds", &lockfile, true);

        assert_eq!(work.len(), 1);
        assert_eq!(work[0].name, "brand_new");
        assert_eq!(cached.len(), 2);
        assert!(cached.contains(&("old_one".to_string(), 111)));
        assert!(cached.contains(&("old_two".to_string(), 222)));
    }

    #[test]
    fn test_split_cached_disabled_treats_everything_as_work() {
        // Studio, Debug and dry-run targets copy every file every time.
        let pending = vec![raw_pending("a", "hash_a"), raw_pending("b", "hash_b")];
        let mut lockfile = Lockfile::default();
        lockfile.set("sounds", "hash_a".to_string(), 111);

        let (work, cached) = split_cached(&pending, "sounds", &lockfile, false);

        assert_eq!(work.len(), 2);
        assert!(cached.is_empty());
    }

    #[test]
    fn test_split_cached_ignores_other_inputs() {
        // A matching hash under a different input name must not be reused.
        let pending = vec![raw_pending("a", "hash_a")];
        let mut lockfile = Lockfile::default();
        lockfile.set("other_input", "hash_a".to_string(), 111);

        let (work, cached) = split_cached(&pending, "sounds", &lockfile, true);

        assert_eq!(work.len(), 1);
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn test_cached_audio_skips_transcode_and_reupload() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wav_path = tmp.path().join("boom.wav");
        let wav = make_wav(&sine_samples(0.3, 22050), 2, 22050);
        std::fs::write(&wav_path, &wav).unwrap();

        let options = ResolvedAudioOptions {
            profile: VorbisProfile::Quality(5.0),
            normalize: None,
        };
        // Cache key is source bytes + settings fingerprint, computed before
        // any transcoding happens.
        let key = hash_parts(&[&wav, options.fingerprint().as_bytes()]);

        let mut lockfile = Lockfile::default();
        lockfile.set("sounds", key, 777);

        let output_path = tmp.path().join("sounds.luau");
        let creator = Creator::User(UserCreator {
            user_id: "1".to_string(),
        });

        // No client is provided, so an upload attempt would emit ID 0.
        // Seeing 777 in the codegen proves the lockfile short-circuit ran
        // before any transcode/upload work.
        let errors = process_raw(
            "sounds",
            vec![wav_path],
            &tmp.path().to_string_lossy().replace('\\', "/"),
            output_path.to_str().unwrap(),
            "flat",
            true,
            false,
            None,
            Some(&options),
            Target::Cloud,
            false,
            &creator,
            None,
            &None,
            &None,
            &None,
            &mut lockfile,
            &mut None,
            1,
            &HashMap::new(),
        )
        .await;

        assert_eq!(errors, 0);
        let luau = std::fs::read_to_string(&output_path).unwrap();
        assert!(luau.contains("rbxassetid://777"), "{}", luau);
    }

    #[tokio::test]
    async fn test_changed_audio_settings_miss_the_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wav_path = tmp.path().join("boom.wav");
        let wav = make_wav(&sine_samples(0.3, 22050), 2, 22050);
        std::fs::write(&wav_path, &wav).unwrap();

        // Lockfile entry was recorded with different settings.
        let old_options = ResolvedAudioOptions {
            profile: VorbisProfile::Quality(5.0),
            normalize: None,
        };
        let old_key = hash_parts(&[&wav, old_options.fingerprint().as_bytes()]);
        let mut lockfile = Lockfile::default();
        lockfile.set("sounds", old_key, 777);

        let new_options = ResolvedAudioOptions {
            profile: VorbisProfile::Bitrate(96),
            normalize: None,
        };

        let output_path = tmp.path().join("sounds.luau");
        let creator = Creator::User(UserCreator {
            user_id: "1".to_string(),
        });

        let errors = process_raw(
            "sounds",
            vec![wav_path],
            &tmp.path().to_string_lossy().replace('\\', "/"),
            output_path.to_str().unwrap(),
            "flat",
            true,
            false,
            None,
            Some(&new_options),
            Target::Cloud,
            false,
            &creator,
            None,
            &None,
            &None,
            &None,
            &mut lockfile,
            &mut None,
            1,
            &HashMap::new(),
        )
        .await;

        assert_eq!(errors, 0);
        let luau = std::fs::read_to_string(&output_path).unwrap();
        // Cache must miss: without a client the fresh upload records ID 0.
        assert!(luau.contains("rbxassetid://0"), "{}", luau);
    }
}
