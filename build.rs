use std::{env, fs, path::{Path, PathBuf}};

fn find_ffmpeg_root(manifest: &Path) -> Option<PathBuf> {
    if let Ok(v) = env::var("FFMPEG_ROOT") {
        let p = PathBuf::from(v);
        if p.join("include/libavcodec/avcodec.h").exists() { return Some(p); }
    }
    let sdk = manifest.join("ffmpeg-sdk");
    let entries = fs::read_dir(sdk).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.join("include/libavcodec/avcodec.h").exists() { return Some(p); }
    }
    None
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = find_ffmpeg_root(&manifest).unwrap_or_else(|| {
        panic!("FFmpeg SDK not found. Run scripts/get_ffmpeg_sdk.ps1 or set FFMPEG_ROOT.")
    });
    let include = root.join("include");

    println!("cargo:rerun-if-changed=native/ffmpeg_shim.c");
    println!("cargo:rerun-if-changed=VideoThumb.ini");
    println!("cargo:rerun-if-env-changed=FFMPEG_ROOT");
    println!("cargo:rustc-env=VIDEOTHUMB_FFMPEG_ROOT={}", root.display());

    let mut b = cc::Build::new();
    b.file("native/ffmpeg_shim.c");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Treat vendor headers as external while keeping our tiny shim at /W4.
        b.flag("/W4");
        b.flag("/external:W0");
        b.flag(&format!("/external:I{}", include.display()));
    } else {
        b.include(&include);
        b.warnings(true);
    }
    b.compile("videothumb_ffmpeg_shim");
}
