# VideoThumb Rust

VideoThumb Rust is a Rust rewrite of the VideoThumb Total Commander WLX video thumbnail plugin.

It is not a line-by-line port of the C++ implementation. The plugin was redesigned and optimized for Rust while preserving compatibility with the external `VideoThumb.ini` configuration format and the FFmpeg 9 shared runtime DLL set.

The implementation uses Rust-specific memory management, caching, bounded parallelism, ZIP image-sequence handling, smart frame selection, dynamic FFmpeg loading, and in-memory FFmpeg image decoding.

The project is fully independent from the C++ VideoThumb repository. It can be cloned, built, installed, and used on its own.

## Version

Current version: **1.0.0**

VideoThumb Rust is a production-ready implementation intended for everyday use in Total Commander.

The C++ and Rust editions share the same purpose and configuration format, but their internal architectures are independent.

## Features

- Fast video thumbnail generation for Total Commander
- Direct FFmpeg library integration
- UNC and SMB network path support
- Works through local directory symlinks pointing to SMB/UNC shares
- In-memory thumbnail cache
- Configurable cache limits
- Bounded parallel decoding
- Configurable FFmpeg decoder thread count
- Decode timeout protection
- Packet processing limits
- Smart representative-frame selection
- Black-frame rejection
- Dark title-card rejection
- Uniform and single-color frame rejection
- Fade and transition rejection
- Multiple fallback seek positions
- ZIP-based image-sequence support
- Natural image ordering inside ZIP sequences
- HEIC/HEIF decoding through FFmpeg
- No dependency on Windows HEIF/HEIC codecs
- No temporary image files
- Configurable ZIP resource limits
- 64-bit Total Commander WLX support

## Architecture

Most of VideoThumb Rust is implemented directly in Rust:

- WLX exports
- configuration handling
- thumbnail cache
- bounded parallelism
- ZIP processing
- smart frame selection
- bitmap creation
- dynamic FFmpeg loading
- in-memory FFmpeg image decoding
- error containment across the plugin boundary

A small C shim is located at:

```text
native/ffmpeg_shim.c
```

It is compiled against the FFmpeg headers and exposes a small set of accessors for FFmpeg structures and constants that are inconvenient to access directly through Rust FFI.

The shim does not perform video decoding and does not statically link FFmpeg.

FFmpeg DLL functions are loaded dynamically at runtime.

## FFmpeg

VideoThumb Rust uses FFmpeg 9 shared libraries.

The required runtime DLLs are:

```text
avcodec-*.dll
avformat-*.dll
avutil-*.dll
swscale-*.dll
swresample-*.dll
```

The release build places these DLLs next to `VideoThumb.wlx64`.

FFmpeg is used for normal video decoding and as an in-memory fallback decoder for image formats that are not handled directly by the Rust image decoder.

## ZIP image sequences

Some files with video extensions are actually ZIP archives containing image sequences.

VideoThumb detects these files and selects an appropriate image directly from the archive.

Candidate images are processed in natural filename order.

Supported image formats include:

- JPG / JPEG
- PNG
- BMP
- GIF
- TIFF
- WebP
- HEIC
- HEIF

Common image formats are decoded first with the Rust `image` crate.

If that decoder cannot handle the image, VideoThumb falls back to FFmpeg using a custom in-memory AVIO context.

HEIC and HEIF images are therefore decoded directly through the bundled FFmpeg libraries.

No Windows HEIF/HEIC codec or Microsoft HEIF Image Extensions package is required.

No temporary files are created during ZIP image decoding.

Resource limits for ZIP processing are configurable through `VideoThumb.ini`.

## Smart frame selection

The first decodable video frame is often a poor thumbnail.

VideoThumb therefore evaluates candidate frames and attempts to reject frames such as:

- black frames
- nearly black frames
- dark title cards
- nearly uniform frames
- single-color frames
- fades
- transitions

If the initial frame is unsuitable, additional configured positions are tested.

The best available representative frame is then used as the thumbnail.

## Configuration

VideoThumb uses:

```text
VideoThumb.ini
```

The configuration format is compatible with the C++ implementation.

Example:

```ini
[Performance]
CacheMB=96
CacheEntries=256
MaxParallel=2
DecoderThreads=2
TimeoutMs=12000
PacketLimit=5000

[Thumbnail]
SeekMs=1000

BlackFrameDetection=1
BlackThreshold=20
BlackPixelPercent=95

TitleCardDetection=1
TitleCardDarkPercent=70
TitleCardMidtoneMaxPercent=20
TitleCardMeanLumaMax=70

UniformFrameDetection=1
MaxLumaStdDev=18
MaxColorStdDev=16
DominantColorPercent=82

TransitionDetection=1
MinEdgePercent=2
EdgeThreshold=20
MaxMeanGradient=7

FallbackSeekMs=3000,5000,10000
FallbackPercent=10

[ZipSequence]
Enabled=1
MaxEntryMB=64
MaxCandidates=16
MaxSourceMP=100

[Compatibility]
ResolveSymlinkFallback=0
```

### Performance

`CacheMB`

Maximum approximate memory size of the thumbnail cache.

`CacheEntries`

Maximum number of cached thumbnails.

`MaxParallel`

Maximum number of thumbnail decoding operations running in parallel.

`DecoderThreads`

Number of FFmpeg decoder threads used for each decoding operation.

`TimeoutMs`

Maximum allowed decoding time for one thumbnail request.

`PacketLimit`

Maximum number of FFmpeg packets processed while searching for a thumbnail frame.

### Thumbnail selection

`SeekMs`

Initial seek position in milliseconds.

`BlackFrameDetection`

Enables rejection of black or nearly black frames.

`BlackThreshold`

Luma threshold used by black-frame detection.

`BlackPixelPercent`

Percentage of pixels that must be below the threshold for the frame to be considered black.

`TitleCardDetection`

Enables rejection of dark title-card-like frames.

`TitleCardDarkPercent`

Required percentage of dark pixels.

`TitleCardMidtoneMaxPercent`

Maximum allowed percentage of midtone pixels.

`TitleCardMeanLumaMax`

Maximum mean luma for title-card detection.

`UniformFrameDetection`

Enables rejection of visually uniform frames.

`MaxLumaStdDev`

Maximum allowed luma standard deviation for a frame considered uniform.

`MaxColorStdDev`

Maximum allowed color standard deviation.

`DominantColorPercent`

Percentage required for a dominant color to classify a frame as nearly single-color.

`TransitionDetection`

Enables rejection of fades and low-detail transition frames.

`MinEdgePercent`

Minimum required percentage of edge pixels.

`EdgeThreshold`

Gradient threshold used for edge detection.

`MaxMeanGradient`

Maximum mean gradient used when identifying transition-like frames.

`FallbackSeekMs`

Additional seek positions tested when the initial frame is unsuitable.

`FallbackPercent`

Fallback seek position expressed as a percentage of video duration.

### ZIP image sequences

`Enabled`

Enables ZIP image-sequence detection.

`MaxEntryMB`

Maximum uncompressed size of a candidate ZIP entry.

`MaxCandidates`

Maximum number of image candidates considered from a ZIP archive.

`MaxSourceMP`

Maximum source image size in megapixels accepted for ZIP image decoding.

### Compatibility

`ResolveSymlinkFallback`

Optional Win32 symlink-resolution fallback.

This is disabled by default because native FFmpeg path handling is normally preferable and avoids unnecessary blocking Win32 path-resolution operations.

## Build prerequisites

The project requires:

1. Rust stable **1.88 or newer**
2. `x86_64-pc-windows-msvc` Rust target
3. Visual Studio 2022 C++ Build Tools
4. PowerShell
5. Internet access for the initial FFmpeg SDK download, unless an FFmpeg SDK is already available

The project currently uses the stable `zip` crate version `8.6.0`.

`Cargo.lock` is committed to keep application/plugin builds reproducible.

## Building

The easiest way to build VideoThumb Rust is:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\build_release.ps1
```

The build script searches for an FFmpeg SDK in this order:

1. `FFMPEG_ROOT` environment variable
2. local `.cargo/config.toml`
3. this project's own `ffmpeg-sdk` directory
4. automatic download into this project's own `ffmpeg-sdk` directory

The project does not depend on the C++ VideoThumb repository or any other local project.

### Optional FFMPEG_ROOT environment variable

An existing FFmpeg SDK can be selected using:

```powershell
$env:FFMPEG_ROOT = "C:\path\to\ffmpeg-sdk"
```

### Optional local Cargo configuration

A local `.cargo/config.toml` can also be used:

```toml
[env]
FFMPEG_ROOT = "C:/path/to/ffmpeg-sdk"
```

This file is intentionally excluded from version control because it normally contains a machine-specific path.

### Automatic FFmpeg SDK download

If no usable FFmpeg SDK is found, `build_release.ps1` automatically runs:

```powershell
.\scripts\get_ffmpeg_sdk.ps1
```

The SDK is downloaded into:

```text
ffmpeg-sdk\
```

This directory is excluded from version control.

## Manual build

Once an FFmpeg SDK is available, the plugin can also be built directly with Cargo:

```powershell
cargo build --release
```

The resulting DLL is:

```text
target\release\videothumb_rs.dll
```

For Total Commander it must be deployed as:

```text
VideoThumb.wlx64
```

The release build script performs this step automatically.

## Release output

Running:

```powershell
.\scripts\build_release.ps1
```

creates:

```text
dist\
```

The directory contains:

```text
VideoThumb.wlx64
VideoThumb.ini
avcodec-*.dll
avformat-*.dll
avutil-*.dll
swscale-*.dll
swresample-*.dll
```

## Installation

Close Total Commander before replacing plugin files.

The included installation script can be used:

```powershell
.\scripts\install_to_totalcmd.ps1
```

The default installation directory is:

```text
<Total Commander>\Plugins\wlx\VideoThumb-Rust\
```

Register:

```text
VideoThumb.wlx64
```

as a WLX plugin in Total Commander.

The Rust and C++ implementations can be installed side-by-side if desired.

## Network paths

VideoThumb is designed to work with:

- local files
- UNC paths
- SMB network shares
- local directory symbolic links whose targets are UNC/SMB shares

FFmpeg accesses the supplied path directly whenever possible.

This avoids unnecessary path translation layers and allows the same decoding engine to be used for both local and network files.

## Performance

VideoThumb is designed to keep thumbnail browsing responsive.

Performance-related techniques include:

- bounded parallel decoding
- configurable decoder threads
- in-memory LRU-style thumbnail caching
- bounded ZIP candidate selection
- limited packet processing
- decoding timeouts
- direct memory decoding
- avoiding temporary files
- compact frame statistics
- early rejection of unsuitable frames

The defaults are intentionally conservative enough for interactive Total Commander use while still providing good throughput.

## Security

FFmpeg decoding runs in-process inside Total Commander.

Rust substantially reduces the amount of memory-unsafe application code, but VideoThumb still uses:

- native FFmpeg DLLs
- a small C shim
- Windows APIs
- FFI boundaries

Therefore the plugin is not sandboxed.

Malformed media files are processed by native FFmpeg code inside the Total Commander process.

Keeping the bundled FFmpeg libraries up to date is recommended.

## Repository layout

```text
.cargo/
native/
scripts/
src/
Cargo.toml
Cargo.lock
build.rs
VideoThumb.ini
README.md
LICENSE
```

Generated or machine-specific directories such as the following are excluded from version control:

```text
target/
dist/
ffmpeg-sdk/
src_prev/
.cargo/config.toml
```

## Related project

The original C++ implementation is maintained separately as:

**VideoThumb**

Both editions provide the same general Total Commander thumbnail functionality and use compatible configuration files, while their internal implementations are independent.

## License

VideoThumb Rust is released under the MIT License.

Copyright (c) 2026 Gonda Valér

FFmpeg and Rust dependencies remain subject to their respective licenses.
