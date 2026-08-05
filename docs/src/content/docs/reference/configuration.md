---
title: Configuration Reference
description: A reference for every field in tungsten.toml.
---

Tungsten is configured through a `tungsten.toml` file in your project's root directory. Run `tungsten init` to generate one, or create it manually.

## Full example

```toml title="tungsten.toml"
[creator]
type = "user"
id = 12345678

[codegen]
style = "nested"
strip_extension = true
ts_declaration = true

# Global upload concurrency (optional, default: 10)
max_concurrent_uploads = 10

# Studio-specific configuration (optional)
[studio]
studio_path = "C:/Program Files/Roblox"  # Optional: override auto-detection
auto_route_version = true  # Optional: fetch latest Studio version automatically

# Example: UI Icons
[inputs.icons]
path = "assets/icons/**/*"
output_path = "src/Icons.luau"
packable = true
uniform_cells = true  # Optional: uniform cell grid packing (default: true)
cell_size = 128       # Optional: exact cell size, or [width, height] (default: largest sprite)
cell_fit = "contain"  # Optional: contain (default), cover, or stretch
svg_scale = 2.0
bleed = true  # Optional: enable alpha bleeding (default: true)

# Example: Large backgrounds
[inputs.backgrounds]
path = "assets/backgrounds/**/*"
output_path = "src/Backgrounds.luau"
packable = false
bleed = false  # Disable alpha bleeding for full-frame backgrounds

# Large backgrounds mean a lot of data, so...
# ...compress them!
[inputs.backgrounds.compress_options]
jpeg_quality = 75
png_quality = 50
keep_metadata = false

# Example: Audio and Models
[inputs.audio]
path = "assets/audio/**/*"
output_path = "src/Audio.luau"

# Transcode every audio file to OGG Vorbis before upload
[inputs.audio.audio]
vorbis_quality = 5.0

# ...and normalize loudness while we're at it
[inputs.audio.audio.normalize]
target_lufs = -14.0
true_peak_ceiling = -1.0

[inputs.models]
path = "assets/models/**/*"
output_path = "src/Models.luau"
```

## Root-level Fields

### `max_concurrent_uploads`

Controls the maximum number of simultaneous uploads to the Roblox Open Cloud API.

| Field                    | Type     | Default | Description                                                                                 |
| ------------------------ | -------- | ------- | ------------------------------------------------------------------------------------------- |
| `max_concurrent_uploads` | `number` | `10`    | Increase for faster uploads on high-bandwidth connections; decrease if hitting rate limits. |

### `[studio]`

Configures Studio sync behavior. All fields are optional.

| Field                | Type      | Description                                                                                                                                                         |
| -------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `studio_path`        | `string`  | Base path to Roblox installation (where `Versions` folder lives). If omitted, Tungsten auto-detects.                                                                |
| `auto_route_version` | `boolean` | If `true` and `studio_path` is set, fetches the latest Studio version from `https://setup.roblox.com/versionQTStudio` and appends `Versions/<version>` to the path. |

---

### `[creator]`

Defines which Roblox account or group assets are uploaded under.

| Field  | Type                  | Description                                                      |
| ------ | --------------------- | ---------------------------------------------------------------- |
| `type` | `"user"` or `"group"` | Whether to upload under a user or a group, defaults to `"user"`. |
| `id`   | `number`              | The Roblox user or group ID to upload under.                     |

---

### `[codegen]`

Controls how Tungsten generates your Luau output files.

| Field             | Type                   | Description                                                               |
| ----------------- | ---------------------- | ------------------------------------------------------------------------- |
| `style`           | `"flat"` or `"nested"` | The structure of the generated Luau table, defaults to `"flat"`.          |
| `strip_extension` | `boolean`              | Whether to strip the file extension from asset keys, defaults to `false`. |
| `ts_declaration`  | `boolean`              | Whether to generate a TypeScript definition file, defaults to `false`.    |

---

### `[inputs.<name>]`

Defines a set of assets to sync. You can define as many input blocks as you need — each one is identified by its name (e.g. `[inputs.packed_assets]`).

| Field           | Type                  | Description                                                                                                    |
| --------------- | --------------------- | -------------------------------------------------------------------------------------------------------------- |
| `path`          | `string`              | A glob pattern pointing to the assets to sync.                                                                 |
| `output_path`   | `string`              | Where Tungsten writes the generated Luau file.                                                                 |
| `packable`      | `boolean`             | Whether to pack matched assets into a spritesheet before uploading.                                            |
| `uniform_cells` | `boolean`             | (Optional) Resample every packed sprite to one exact cell size and pack on a fixed grid, defaults to `true`.   |
| `cell_size`     | `number` or `[w, h]`  | (Optional) Exact cell dimensions for uniform packing. Defaults to the largest sprite width/height in the input. |
| `cell_fit`      | `string`              | (Optional) `"contain"` (default), `"cover"`, or `"stretch"` — how sprites are resampled into the cell.        |
| `svg_scale`     | `number`              | (Optional) Multiplier for SVG rasterization, defaults to 1.0.                                                  |
| `bleed`         | `boolean`             | (Optional) Enable alpha bleeding to prevent edge artifacts, defaults to `true`.                                |
| `type`          | `string`              | The Roblox asset type (e.g., `decal`, `image`, `audio`, `model`). Overrides type inferred from file extension. |

:::note
When `packable` is set to `true`, Tungsten packs the matched images into a spritesheet on the fly before uploading. The spritesheet is never saved to disk.
:::

#### Uniform cell normalization

With `uniform_cells` enabled (the default for packable inputs), every sprite is resampled to one exact cell size before packing, and sprites are placed in name-sorted order on a fixed grid — sprite `k` sits at column `k % columns`, row `k / columns`. This guarantees identical render dimensions for every sprite regardless of source resolution: declare `cell_size = 128` and a stray 200px export can no longer ship as an oversized icon.

```toml
[inputs.icons]
path = "assets/icons/**/*"
output_path = "src/Icons.luau"
packable = true
cell_size = 128        # every sprite becomes exactly 128x128
cell_fit = "contain"   # aspect preserved, centered on transparent padding
```

Cells may not exceed 1024×1024 (the maximum sheet size). Set `uniform_cells = false` to restore free-form rect packing.

---

### `[inputs.<name>.compress_options]`

Enables and configures image optimization for a specific input group. When this table is present, Tungsten uses `libcaesium` to reduce the file size of your images before they are uploaded to Roblox.

| Field           | Type      | Description                                                           |
| --------------- | --------- | --------------------------------------------------------------------- |
| `jpeg_quality`  | `number`  | Quality of the JPEG image (0-100), defaults to 80.                    |
| `png_quality`   | `number`  | Quality of the PNG image (0-100), defaults to 80.                     |
| `keep_metadata` | `boolean` | Whether to keep metadata in the compressed image, defaults to `true`. |

---

### `[inputs.<name>.audio]`

Enables audio transcoding for a specific input group. When this table is present, every audio file (WAV, MP3, M4A/AAC, FLAC) is decoded and re-encoded to OGG Vorbis before upload — pure Rust, no ffmpeg required. Sources that are already OGG are passed through untouched.

This also lets your repository hold lossless masters: Roblox won't accept a WAV upload, but with transcoding enabled Tungsten converts it at build time, the same source/artifact split it already applies to SVG.

| Field            | Type     | Description                                                                        |
| ---------------- | -------- | ----------------------------------------------------------------------------------- |
| `vorbis_quality` | `number` | Vorbis VBR quality, `-1.0` to `10.0`. Defaults to `5.0` (roughly 160 kbps stereo). |
| `bitrate`        | `number` | Target VBR bitrate in kbps. Takes precedence over `vorbis_quality`.                |

An empty `[inputs.<name>.audio]` table enables transcoding with the defaults.

:::note
Without this section, WAV, FLAC and M4A files are uploaded as-is — and Roblox rejects them. Tungsten logs a warning when this happens.
:::

---

### `[inputs.<name>.audio.normalize]`

Enables loudness normalization for a specific input group. Integrated loudness is measured per **EBU R128 / ITU-R BS.1770-4** (K-weighted, gated at −70 LUFS absolute and −10 LU relative) and gain is applied to hit the target.

Roblox does no server-side loudness normalization the way streaming platforms do, so the build tool is the only layer where this can happen. Normalizing at build time makes `Sound.Volume = 1` mean one consistent thing across your entire project — no more hand-tuning volume at dozens of call sites.

| Field               | Type     | Description                                                                                                            |
| ------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------ |
| `target_lufs`       | `number` | Integrated loudness target in LUFS. Defaults to `-14.0` (the common streaming loudness target).                          |
| `true_peak_ceiling` | `number` | True peak ceiling in dBTP. Gain is capped so output never exceeds this, even if the loudness target is missed. Defaults to `-1.0`. |

An empty `[inputs.<name>.audio.normalize]` table enables normalization with the defaults, and implies transcoding: normalization requires decoding to PCM, so OGG sources are re-encoded too (they are otherwise passed through untouched). Streams that are effectively silent (below the −70 LUFS gate) are left unmodified.

Suggested targets:

- `-14.0` — matches Spotify/YouTube loudness; good general default for game audio
- `-16.0` — slightly more headroom; common for voice-heavy content
- `-23.0` — EBU R128 broadcast target; conservative, lots of headroom
