# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v3.2.2]

### Added

- A `Transcoding` progress bar over the audio decode/normalize/encode pass, drawn as soon as work starts so a single long file is no longer silent, plus one info line reporting the file count and the loudness target when normalization is enabled

### Fixed

- Upload progress counted cached assets, so adding one sound to a folder of thirty reported `31/31` uploading when only one file was sent. Lockfile hits are now resolved before the bar is sized, the counter shows `1/1`, and the skipped count is reported once
- Progress bar columns were padded on already-colorized strings, so the ANSI escape bytes counted toward the field width and no visible padding was emitted — phase and item columns never aligned
- The completion line was drawn over the longer in-progress line with only a carriage return, leaving that line's tail visible right after the counter (`3/3NeonCitySecretRewardQuiet`)
- Long item names in the progress bar are truncated on character boundaries, so multi-byte names can no longer be split mid-character

## [v3.2.1]

### Fixed

- Processed audio re-uploaded on every sync: the upload cache keyed on encoded OGG bytes, which vary per encode due to randomized Ogg stream serials. Cache now keys on source bytes plus a settings fingerprint, checked before transcoding — unchanged files skip the decode/encode round-trip and the upload entirely. Existing audio lockfile entries re-upload once, then cache correctly
- Flaky test race on the shared `.env` file that intermittently failed CI

### Changed

- Dependencies are compiled at opt-level 3 in dev builds; audio and image codecs were unusably slow at opt-level 0
- Upload operation polling caps its backoff at 8 seconds (window still ~17 minutes), so finished audio moderation is noticed promptly
- Per-file transcode timing is logged at debug level (`--verbose`)
- Dry runs skip audio transcoding since the bytes are never uploaded or copied

## [v3.2.0]

### Added

- Uniform cell normalization for packed inputs: every sprite is resampled to one exact cell size and packed on a fixed grid in name-sorted order, so sprite `k` always sits at `(k % columns, k / columns)` and renders at identical dimensions regardless of source resolution
- `uniform_cells` (default: `true`), `cell_size` (single number or `[width, height]`) and `cell_fit` (`contain`/`cover`/`stretch`) input configuration fields
- OGG transcoding via `[inputs.<name>.audio]`: decodes WAV, MP3, M4A/AAC and FLAC via symphonia and re-encodes to OGG Vorbis via vorbis_rs — pure Rust, no ffmpeg dependency; OGG sources are passed through untouched
- `vorbis_quality` (libvorbis `-1.0..=10.0` scale, default 5.0) and `bitrate` (kbps VBR target) audio encoder options
- Audio loudness normalization via `[inputs.<name>.audio.normalize]`: measures integrated loudness per EBU R128 / ITU-R BS.1770-4 (K-weighted, gated at -70 LUFS absolute and -10 LU relative) and applies gain to hit `target_lufs` (default -14.0), capped by `true_peak_ceiling` (default -1.0 dBTP)
- `m4a`/`aac` audio extensions are now recognized
- Warning when uploading audio formats Roblox rejects (WAV, FLAC, M4A) without transcoding enabled

### Changed

- **Breaking-ish:** packable inputs now default to uniform-cell grid packing; set `uniform_cells = false` per input to restore free-form rect packing

### Fixed

- Packed inputs generated an empty Luau table on every target since v3.1.0: a shadowed vec dropped all sprite codegen entries before they reached `write_codegen`

## [v3.1.0]

### Added

- Web asset support: `WebAsset` type with automatic animation detection for `rbxm`/`rbxmx`/`fbx` files
- Optional `asset_type` and web asset mapping in input configuration, with per-input `asset_type` override resolution propagated to all sync processors
- Web asset support in individual, packed, and raw sync processors, all sharing a common compress helper
- `seed_web_assets` codegen helper and simplified `write_codegen` output
- Rewritten `init` command with interactive prompts for creator, folder selection, and codegen output
- Interactive folder selector in `init` with type cycling and multi-select fallback
- Global `--verbose` flag that enables debug-level log output for troubleshooting
- Debug logging across upload, sync, init, watch, config parsing, and API key resolution paths

### Changed

- Log formatting updated to colored backgrounds and section headers
- Capitalized section headers in the test and watch commands
- Replaced loose per-image parameters in individual sync with a `ProcessImageCtx` context struct
- Moved `maybe_compress_png` to the public API and removed the unused parallel variant
- Bumped `clap` to 4.6.5 and `resvg`/`usvg` to 0.48
- Capitalized the app name in help output

### Fixed

- Punctuation in CLI error messages (removed stray trailing period)

### Removed

- LucideRoblox origin note and Windows-only disclaimer from README

## [v3.0.0]

### Added

- Configurable upload concurrency via `max_concurrent_uploads` in `tungsten.toml` (default: 10)
- Studio sync cleanup logic to remove stale files from `.tungsten-debug/` folder
- File tracking mechanism for Studio sync to track expected files during sync operations
- `StudioConfig` section with `studio_path` and `auto_route_version` for advanced Studio path handling
- Automatic version routing via `https://setup.roblox.com/versionQTStudio` when `auto_route_version` is enabled

### Changed

- Changed Studio sync behavior from wiping previous contents to incremental sync that preserves assets between Studio version updates
- Updated SVG scaling to use per-file scale based on viewBox rather than global input scale
- Changed log output format from symbols (∙, ✓, ⚠, ✗) to bracketed labels ([INFO], [SUCCESS], [WARNING], [ERROR]) for better readability
- Changed progress bar format: removed leading spaces, added zero-padded counters aligned to total width, and updated completion line to use [SUCCESS] label
- Changed API key loading to use standard `.env` files with `TUNGSTEN_API_KEY` instead of `tungsten_api_key.env` files
- Improved variable naming in `src/core/assets/img/alpha_bleed.rs` for BFS algorithm readability
- Added bleed configuration option to inputs to control alpha bleeding (defaults to true for backward compatibility)
- Implemented automatic spritesheet packing similar to Adobe Animate:
  - Sorts sprites by largest height first, then largest width first
  - Uses rect packing algorithm with upright-only sprite placement
  - Dynamically sizes atlases (calculates needed size, increases only when necessary)
  - Enforces maximum atlas size of 1024x1024
  - Automatically generates additional atlases when needed
  - Trims final atlases to actual used space (removes empty padding)
- Modified spritesheet packing to always use maximum atlas size (1024x1024) to minimize the number of sheets while trimming unused space
- Optimized alpha bleeding algorithm (`alpha_bleed.rs`) — ~10-50x faster for spritesheets via bit-packed `Vec<u32>`, ring buffer, and 4-neighbor fast path
- Parallel DPI variant pre-processing and upload (2x, 3x via Rayon)
- Parallel spritesheet bleed/encode/compress for multiple atlases
- Lockfile hashing optimization using `hex::encode()` for SHA-256 digest formatting

### Fixed

- Fixed studio sync incorrectly changing file extensions for audio and model assets (e.g., .mp3 to .audio, .rbxm to .model) when syncing to Studio target

### Removed

- Removed adding `tungsten_api_key.env` into the project's `.gitignore` as it's no longer being used
- Removed DPI variant packing support; high DPI variants are skipped for packing (waitlisted for manual upload) but still generate DPI group code entries

## [v3.0.0-rc.2]

### Added

- Added studio sync cleanup logic to remove stale files from `.tungsten-debug/` folder
- Added file tracking mechanism for Studio sync to track expected files during sync operations
- Added `StudioConfig` section with `studio_path` and `auto_route_version` for advanced Studio path handling
- Added automatic version routing via `https://setup.roblox.com/versionQTStudio` when `auto_route_version` is enabled

### Changed

- Changed Studio sync behavior from wiping previous contents to incremental sync that preserves assets between Studio version updates
- Updated SVG scaling to use per-file scale based on viewBox rather than global input scale
- Changed log output format from symbols (∙, ✓, ⚠, ✗) to bracketed labels ([INFO], [SUCCESS], [WARNING], [ERROR]) for better readability
- Changed progress bar format: removed leading spaces, added zero-padded counters aligned to total width, and updated completion line to use [SUCCESS] label
- Changed API key loading to use standard `.env` files with `TUNGSTEN_API_KEY` instead of `tungsten_api_key.env` files

### Fixed

- Fixed studio sync incorrectly changing file extensions for audio and model assets (e.g., .mp3 to .audio, .rbxm to .model) when syncing to Studio target

### Removed

- Removed adding `tungsten_api_key.env` into the project's `.gitignore` as it's no longer being used.

## [v3.0.0-rc.1]

### Added

- CHANGELOG.md file to track changes between versions
- **Breaking:** Added required `type` field to inputs, allowing specifying asset type (e.g., decal, image) independent of file kind.

### Changed

- Improved variable naming in `src/core/assets/img/alpha_bleed.rs` for better readability:
  - Replaced single-letter variables (`w`, `h`, `len`, `i`, `x`, `y`) with descriptive names (`width`, `height`, `pixel_count`, `index`, `x`, `y`)
  - Improved clarity in BFS algorithm with more descriptive variable names (`red_sum`, `green_sum`, `blue_sum`, `sample_count`)
  - Renamed queue variables for clarity (`current` -> `current_wave`, `next` -> `next_wave`)
- Updated ignore reason in `src/core/assets/img/convert.rs` test:
  - Added descriptive reason to ignored test: `#[ignore = "TGA support not fully tested in CI environment"]`
- Improved documentation accuracy:
  - Fixed creator configuration example in docs/getting-started/first-sync.mdx to show correct `[creator]` format
  - Corrected debug folder naming in docs/reference/cli.md from .tungsten_debug to .tungsten-debug to match implementation
  - Enhanced meta file documentation in docs/reference/meta-files.mdx to explain the naming convention priority:
    - For files: tries `name.format.tmeta` first (e.g., `logo.png.tmeta`), then falls back to `name.tmeta` (e.g., `logo.tmeta`)
    - For directories: uses `name.tmeta` (e.g., `icons.tmeta`)
  - Improved meta file handling in src/core/assets/asset.rs to implement the dual naming convention:
    - Files check for `name.format.tmeta` first, then `name.tmeta`
    - Directories use `name.tmeta`
    - Added comprehensive tests for meta file naming behavior
- Added bleed configuration option to inputs to control alpha bleeding processing (defaults to true for backward compatibility)
- Implemented automatic spritesheet packing similar to Adobe Animate:
  - Sorts sprites by largest height first, then largest width first
  - Uses rect packing algorithm with upright-only sprite placement
  - Dynamically sizes atlases (calculates needed size, increases only when necessary)
  - Enforces maximum atlas size of 1024x1024
  - Automatically generates additional atlases when needed
  - Trims final atlases to actual used space (removes empty padding)
- Modified spritesheet packing to always use maximum atlas size (1024x1024) to minimize the number of sheets while trimming unused space.

### Fixed

- No fixes in this release

### Removed

- Removed DPI variant packing support; high DPI variants are skipped for packing (waitlisted for manual upload) but still generate DPI group code entries
