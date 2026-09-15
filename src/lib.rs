#![cfg(windows)]
#![allow(non_camel_case_types)]

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, HashMap},
    ffi::{c_char, c_int, c_void, CString, OsString},
    fs::File,
    io::{Cursor, Read},
    mem,
    os::windows::ffi::{OsStrExt, OsStringExt},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    ptr::{self, null, null_mut},
    slice,
    sync::{Arc, Mutex, OnceLock},
};

use image::{imageops::FilterType, DynamicImage, ImageDecoder, ImageReader};
use zip::ZipArchive;

// ----------------------------- Win32 FFI ----------------------------------

type HANDLE = isize;
type HMODULE = isize;
type HBITMAP = isize;
type HWND = isize;
type BOOL = i32;
type DWORD = u32;
type UINT = u32;
type WORD = u16;
type LONG = i32;

const WAIT_OBJECT_0: DWORD = 0;
const BI_RGB: DWORD = 0;
const DIB_RGB_COLORS: UINT = 0;
const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: DWORD = 0x0000_0100;
const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: DWORD = 0x0000_1000;
const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: DWORD = 0x0000_0002;
const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: DWORD = 0x0000_0004;

#[repr(C)]
struct BITMAPINFOHEADER {
    bi_size: DWORD,
    bi_width: LONG,
    bi_height: LONG,
    bi_planes: WORD,
    bi_bit_count: WORD,
    bi_compression: DWORD,
    bi_size_image: DWORD,
    bi_x_pels_per_meter: LONG,
    bi_y_pels_per_meter: LONG,
    bi_clr_used: DWORD,
    bi_clr_important: DWORD,
}

#[repr(C)]
struct RGBQUAD {
    rgb_blue: u8,
    rgb_green: u8,
    rgb_red: u8,
    rgb_reserved: u8,
}

#[repr(C)]
struct BITMAPINFO {
    bmi_header: BITMAPINFOHEADER,
    bmi_colors: [RGBQUAD; 1],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleExW(flags: DWORD, name_or_addr: *const u16, module: *mut HMODULE) -> BOOL;
    fn GetModuleFileNameW(module: HMODULE, filename: *mut u16, size: DWORD) -> DWORD;
    fn LoadLibraryExW(filename: *const u16, file: HANDLE, flags: DWORD) -> HMODULE;
    fn FreeLibrary(module: HMODULE) -> BOOL;
    fn GetProcAddress(module: HMODULE, name: *const c_char) -> *mut c_void;
    fn CreateSemaphoreW(
        attrs: *const c_void,
        initial: LONG,
        maximum: LONG,
        name: *const u16,
    ) -> HANDLE;
    fn WaitForSingleObject(handle: HANDLE, millis: DWORD) -> DWORD;
    fn ReleaseSemaphore(handle: HANDLE, release_count: LONG, previous: *mut LONG) -> BOOL;
    fn GetTickCount64() -> u64;
    fn GetPrivateProfileIntW(
        section: *const u16,
        key: *const u16,
        default: i32,
        filename: *const u16,
    ) -> UINT;
    fn GetPrivateProfileStringW(
        section: *const u16,
        key: *const u16,
        default: *const u16,
        returned: *mut u16,
        size: DWORD,
        filename: *const u16,
    ) -> DWORD;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateDIBSection(
        hdc: HANDLE,
        info: *const BITMAPINFO,
        usage: UINT,
        bits: *mut *mut c_void,
        section: HANDLE,
        offset: DWORD,
    ) -> HBITMAP;
}

fn wide_z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn path_wide_z(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn own_module() -> Option<HMODULE> {
    let mut h = 0;
    let addr = module_dir as *const () as *const u16;
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            addr,
            &mut h,
        )
    };
    (ok != 0 && h != 0).then_some(h)
}

fn module_path_from_handle(h: HMODULE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetModuleFileNameW(h, buf.as_mut_ptr(), buf.len() as DWORD) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    buf.truncate(n);
    Some(PathBuf::from(OsString::from_wide(&buf)))
}

fn module_dir() -> &'static Path {
    // Called a handful of times total (once for settings, once per FFmpeg
    // DLL probed) during one-time startup, never per-thumbnail -- but the
    // plugin's own directory can't change during the process's lifetime,
    // so there's no reason to repeat the GetModuleHandleExW /
    // GetModuleFileNameW round trip on every call.
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        own_module()
            .and_then(module_path_from_handle)
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    })
}

// ------------------------------ settings ----------------------------------

#[derive(Clone)]
struct Settings {
    cache_mb: i32,
    cache_entries: i32,
    max_parallel: i32,
    decoder_threads: i32,
    seek_ms: i32,
    timeout_ms: i32,
    packet_limit: i32,

    black_frame_detection: bool,
    black_threshold: i32,
    black_pixel_percent: i32,
    title_card_detection: bool,
    title_card_dark_percent: i32,
    title_card_midtone_max_percent: i32,
    title_card_mean_luma_max: i32,
    uniform_frame_detection: bool,
    max_luma_stddev: i32,
    max_color_stddev: i32,
    dominant_color_percent: i32,
    transition_detection: bool,
    min_edge_percent: i32,
    edge_threshold: i32,
    max_mean_gradient: i32,
    fallback_seek_ms: Vec<i32>,
    fallback_percent: i32,

    zip_sequences: bool,
    zip_max_entry_mb: i32,
    zip_max_candidates: i32,
    zip_max_source_mp: i32,
    resolve_symlink_fallback: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cache_mb: 96,
            cache_entries: 256,
            max_parallel: 2,
            decoder_threads: 2,
            seek_ms: 1000,
            timeout_ms: 12000,
            packet_limit: 5000,
            black_frame_detection: true,
            black_threshold: 20,
            black_pixel_percent: 95,
            title_card_detection: true,
            title_card_dark_percent: 70,
            title_card_midtone_max_percent: 20,
            title_card_mean_luma_max: 70,
            uniform_frame_detection: true,
            max_luma_stddev: 18,
            max_color_stddev: 16,
            dominant_color_percent: 82,
            transition_detection: true,
            min_edge_percent: 2,
            edge_threshold: 20,
            max_mean_gradient: 7,
            fallback_seek_ms: vec![3000, 5000, 10000],
            fallback_percent: 10,
            zip_sequences: true,
            zip_max_entry_mb: 64,
            zip_max_candidates: 16,
            zip_max_source_mp: 100,
            resolve_symlink_fallback: false,
        }
    }
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi)
}

fn ini_int(ini: &Path, section: &str, key: &str, default: i32) -> i32 {
    let sec = wide_z(section);
    let key = wide_z(key);
    let file = path_wide_z(ini);
    unsafe { GetPrivateProfileIntW(sec.as_ptr(), key.as_ptr(), default, file.as_ptr()) as i32 }
}

fn ini_string(ini: &Path, section: &str, key: &str, default: &str) -> String {
    let sec = wide_z(section);
    let key = wide_z(key);
    let def = wide_z(default);
    let file = path_wide_z(ini);
    let mut buf = vec![0u16; 256];
    let n = unsafe {
        GetPrivateProfileStringW(
            sec.as_ptr(),
            key.as_ptr(),
            def.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as DWORD,
            file.as_ptr(),
        )
    } as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn load_settings() -> Settings {
    let mut s = Settings::default();
    let ini = module_dir().join("VideoThumb.ini");

    s.cache_mb = clamp(ini_int(&ini, "Performance", "CacheMB", s.cache_mb), 0, 1024);
    s.cache_entries = clamp(
        ini_int(&ini, "Performance", "CacheEntries", s.cache_entries),
        0,
        4096,
    );
    s.max_parallel = clamp(
        ini_int(&ini, "Performance", "MaxParallel", s.max_parallel),
        1,
        16,
    );
    s.decoder_threads = clamp(
        ini_int(&ini, "Performance", "DecoderThreads", s.decoder_threads),
        0,
        32,
    );
    s.seek_ms = clamp(ini_int(&ini, "Thumbnail", "SeekMs", s.seek_ms), 0, 600000);
    s.timeout_ms = clamp(
        ini_int(&ini, "Performance", "TimeoutMs", s.timeout_ms),
        1000,
        120000,
    );
    s.packet_limit = clamp(
        ini_int(&ini, "Performance", "PacketLimit", s.packet_limit),
        100,
        100000,
    );

    s.black_frame_detection = ini_int(
        &ini,
        "Thumbnail",
        "BlackFrameDetection",
        s.black_frame_detection as i32,
    ) != 0;
    s.black_threshold = clamp(
        ini_int(&ini, "Thumbnail", "BlackThreshold", s.black_threshold),
        0,
        255,
    );
    s.black_pixel_percent = clamp(
        ini_int(
            &ini,
            "Thumbnail",
            "BlackPixelPercent",
            s.black_pixel_percent,
        ),
        50,
        100,
    );
    s.title_card_detection = ini_int(
        &ini,
        "Thumbnail",
        "TitleCardDetection",
        s.title_card_detection as i32,
    ) != 0;
    s.title_card_dark_percent = clamp(
        ini_int(
            &ini,
            "Thumbnail",
            "TitleCardDarkPercent",
            s.title_card_dark_percent,
        ),
        40,
        100,
    );
    s.title_card_midtone_max_percent = clamp(
        ini_int(
            &ini,
            "Thumbnail",
            "TitleCardMidtoneMaxPercent",
            s.title_card_midtone_max_percent,
        ),
        0,
        80,
    );
    s.title_card_mean_luma_max = clamp(
        ini_int(
            &ini,
            "Thumbnail",
            "TitleCardMeanLumaMax",
            s.title_card_mean_luma_max,
        ),
        0,
        255,
    );
    s.uniform_frame_detection = ini_int(
        &ini,
        "Thumbnail",
        "UniformFrameDetection",
        s.uniform_frame_detection as i32,
    ) != 0;
    s.max_luma_stddev = clamp(
        ini_int(&ini, "Thumbnail", "MaxLumaStdDev", s.max_luma_stddev),
        0,
        128,
    );
    s.max_color_stddev = clamp(
        ini_int(&ini, "Thumbnail", "MaxColorStdDev", s.max_color_stddev),
        0,
        128,
    );
    s.dominant_color_percent = clamp(
        ini_int(
            &ini,
            "Thumbnail",
            "DominantColorPercent",
            s.dominant_color_percent,
        ),
        40,
        100,
    );
    s.transition_detection = ini_int(
        &ini,
        "Thumbnail",
        "TransitionDetection",
        s.transition_detection as i32,
    ) != 0;
    s.min_edge_percent = clamp(
        ini_int(&ini, "Thumbnail", "MinEdgePercent", s.min_edge_percent),
        0,
        100,
    );
    s.edge_threshold = clamp(
        ini_int(&ini, "Thumbnail", "EdgeThreshold", s.edge_threshold),
        1,
        255,
    );
    s.max_mean_gradient = clamp(
        ini_int(&ini, "Thumbnail", "MaxMeanGradient", s.max_mean_gradient),
        0,
        255,
    );
    s.fallback_percent = clamp(
        ini_int(&ini, "Thumbnail", "FallbackPercent", s.fallback_percent),
        0,
        90,
    );

    let mut parsed = Vec::new();
    for part in ini_string(&ini, "Thumbnail", "FallbackSeekMs", "3000,5000,10000").split(',') {
        if parsed.len() >= 8 {
            break;
        }
        if let Ok(v) = part.trim().parse::<i32>() {
            let v = clamp(v, 0, 600000);
            if v > 0 && !parsed.contains(&v) {
                parsed.push(v);
            }
        }
    }
    s.fallback_seek_ms = parsed;

    s.zip_sequences = ini_int(&ini, "ZipSequence", "Enabled", s.zip_sequences as i32) != 0;
    s.zip_max_entry_mb = clamp(
        ini_int(&ini, "ZipSequence", "MaxEntryMB", s.zip_max_entry_mb),
        1,
        512,
    );
    s.zip_max_candidates = clamp(
        ini_int(&ini, "ZipSequence", "MaxCandidates", s.zip_max_candidates),
        1,
        128,
    );
    s.zip_max_source_mp = clamp(
        ini_int(&ini, "ZipSequence", "MaxSourceMP", s.zip_max_source_mp),
        1,
        1000,
    );
    s.resolve_symlink_fallback = ini_int(
        &ini,
        "Compatibility",
        "ResolveSymlinkFallback",
        s.resolve_symlink_fallback as i32,
    ) != 0;
    s
}

fn settings() -> &'static Settings {
    static S: OnceLock<Settings> = OnceLock::new();
    S.get_or_init(load_settings)
}

// ----------------------------- thumbnail data ------------------------------

#[derive(Clone)]
struct ThumbData {
    width: i32,
    height: i32,
    stride: i32,
    bgra: Vec<u8>,
}

fn make_hbitmap(t: &ThumbData) -> HBITMAP {
    if t.width <= 0 || t.height <= 0 || t.stride < t.width.saturating_mul(4) || t.bgra.is_empty() {
        return 0;
    }
    let bmi = BITMAPINFO {
        bmi_header: BITMAPINFOHEADER {
            bi_size: mem::size_of::<BITMAPINFOHEADER>() as DWORD,
            bi_width: t.width,
            bi_height: -t.height,
            bi_planes: 1,
            bi_bit_count: 32,
            bi_compression: BI_RGB,
            bi_size_image: 0,
            bi_x_pels_per_meter: 0,
            bi_y_pels_per_meter: 0,
            bi_clr_used: 0,
            bi_clr_important: 0,
        },
        bmi_colors: [RGBQUAD {
            rgb_blue: 0,
            rgb_green: 0,
            rgb_red: 0,
            rgb_reserved: 0,
        }],
    };
    let mut bits: *mut c_void = null_mut();
    let hbmp = unsafe { CreateDIBSection(0, &bmi, DIB_RGB_COLORS, &mut bits, 0, 0) };
    if hbmp == 0 || bits.is_null() {
        return 0;
    }
    let row = (t.width as usize) * 4;
    let stride = t.stride as usize;
    if stride == row {
        // Every ThumbData constructor in this file packs rows tightly
        // (stride == width*4, no padding), so this is always the path
        // taken today; one contiguous copy beats `height` separate calls.
        // The per-row fallback stays in case that invariant ever changes.
        unsafe {
            ptr::copy_nonoverlapping(t.bgra.as_ptr(), bits as *mut u8, row * t.height as usize);
        }
    } else {
        for y in 0..t.height as usize {
            unsafe {
                ptr::copy_nonoverlapping(
                    t.bgra.as_ptr().add(y * stride),
                    (bits as *mut u8).add(y * row),
                    row,
                );
            }
        }
    }
    hbmp
}

// -------------------------------- cache -----------------------------------

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: Vec<u16>,
    max_w: i32,
    max_h: i32,
}

// LRU cache. Recency used to be tracked with a VecDeque<CacheKey>: moving an
// entry to the front on every single get/put meant a linear `position()`
// scan plus an O(n) `VecDeque::remove` (shifts everything after it), so a
// cache *hit* -- meant to be the fast path -- cost O(n) in the configured
// entry count (up to 4096, per VideoThumb.ini). Pairing the HashMap with a
// BTreeMap<tick, CacheKey> replaces that with O(log n): the smallest tick
// is always the least-recently-used entry, so eviction is `pop_first()`.
struct ThumbCache {
    map: HashMap<CacheKey, (Arc<ThumbData>, u64)>,
    order: BTreeMap<u64, CacheKey>,
    next_tick: u64,
    total_bytes: usize,
}

impl ThumbCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: BTreeMap::new(),
            next_tick: 0,
            total_bytes: 0,
        }
    }

    fn get(&mut self, key: &CacheKey) -> Option<Arc<ThumbData>> {
        if settings().cache_mb == 0 || settings().cache_entries == 0 {
            return None;
        }
        let new_tick = self.next_tick;
        let (data, old_tick) = {
            let entry = self.map.get_mut(key)?;
            let data = entry.0.clone();
            let old_tick = entry.1;
            entry.1 = new_tick;
            (data, old_tick)
        };
        self.next_tick += 1;
        self.order.remove(&old_tick);
        self.order.insert(new_tick, key.clone());
        Some(data)
    }

    fn put(&mut self, key: CacheKey, data: Arc<ThumbData>) {
        let cfg = settings();
        if cfg.cache_mb == 0 || cfg.cache_entries == 0 {
            return;
        }
        let limit = cfg.cache_mb as usize * 1024 * 1024;
        let n = data.bgra.len();
        if n == 0 || n > limit {
            return;
        }
        if let Some((old_data, old_tick)) = self.map.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old_data.bgra.len());
            self.order.remove(&old_tick);
        }
        let tick = self.next_tick;
        self.next_tick += 1;
        self.total_bytes += n;
        self.order.insert(tick, key.clone());
        self.map.insert(key, (data, tick));
        while self.map.len() > cfg.cache_entries as usize || self.total_bytes > limit {
            let Some((_, victim_key)) = self.order.pop_first() else {
                break;
            };
            if let Some((old_data, _)) = self.map.remove(&victim_key) {
                self.total_bytes = self.total_bytes.saturating_sub(old_data.bgra.len());
            }
        }
    }
}

fn cache() -> &'static Mutex<ThumbCache> {
    static C: OnceLock<Mutex<ThumbCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ThumbCache::new()))
}

fn normalized_cache_path(wide: &[u16]) -> Vec<u16> {
    wide.iter()
        .map(|&c| {
            let c = if c == b'/' as u16 { b'\\' as u16 } else { c };
            if (b'A' as u16..=b'Z' as u16).contains(&c) {
                c + 32
            } else {
                c
            }
        })
        .collect()
}

// -------------------------- bounded parallelism ----------------------------

fn decode_semaphore() -> HANDLE {
    static SEM: OnceLock<HANDLE> = OnceLock::new();
    *SEM.get_or_init(|| unsafe {
        CreateSemaphoreW(
            null(),
            settings().max_parallel,
            settings().max_parallel,
            null(),
        )
    })
}

struct DecodePermit {
    h: HANDLE,
    held: bool,
}
impl DecodePermit {
    fn acquire() -> Self {
        let h = decode_semaphore();
        let held = h != 0
            && unsafe { WaitForSingleObject(h, settings().timeout_ms as DWORD) } == WAIT_OBJECT_0;
        Self { h, held }
    }
}
impl Drop for DecodePermit {
    fn drop(&mut self) {
        if self.held {
            unsafe {
                ReleaseSemaphore(self.h, 1, null_mut());
            }
        }
    }
}

// ----------------------------- FFmpeg FFI ---------------------------------

#[repr(C)]
struct AVFormatContext {
    _private: [u8; 0],
}
#[repr(C)]
struct AVCodecContext {
    _private: [u8; 0],
}
#[repr(C)]
struct AVPacket {
    _private: [u8; 0],
}
#[repr(C)]
struct AVFrame {
    _private: [u8; 0],
}
#[repr(C)]
struct AVCodec {
    _private: [u8; 0],
}
#[repr(C)]
struct AVCodecParameters {
    _private: [u8; 0],
}
#[repr(C)]
struct AVDictionary {
    _private: [u8; 0],
}
#[repr(C)]
struct AVInputFormat {
    _private: [u8; 0],
}
#[repr(C)]
struct AVIOContext {
    _private: [u8; 0],
}
#[repr(C)]
struct SwsContext {
    _private: [u8; 0],
}

extern "C" {
    fn vt_libavutil_major() -> c_int;
    fn vt_libswresample_major() -> c_int;
    fn vt_libavcodec_major() -> c_int;
    fn vt_libavformat_major() -> c_int;
    fn vt_libswscale_major() -> c_int;
    fn vt_av_log_quiet() -> c_int;
    fn vt_avmedia_type_video() -> c_int;
    fn vt_av_pix_fmt_bgra() -> c_int;
    fn vt_averror_eagain() -> c_int;
    fn vt_averror_eof() -> c_int;
    fn vt_averror_einval() -> c_int;
    fn vt_avseek_flag_backward() -> c_int;
    fn vt_avseek_size() -> c_int;
    fn vt_avseek_force() -> c_int;
    fn vt_sws_fast_bilinear() -> c_int;
    fn vt_format_set_interrupt(
        f: *mut AVFormatContext,
        cb: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
        opaque: *mut c_void,
    );
    fn vt_format_set_pb(f: *mut AVFormatContext, pb: *mut AVIOContext);
    fn vt_format_duration(f: *const AVFormatContext) -> i64;
    fn vt_format_nb_streams(f: *const AVFormatContext) -> u32;
    fn vt_format_stream(f: *mut AVFormatContext, i: u32) -> *mut c_void;
    fn vt_stream_codecpar(s: *mut c_void) -> *mut AVCodecParameters;
    fn vt_codecpar_width(p: *const AVCodecParameters) -> c_int;
    fn vt_codecpar_height(p: *const AVCodecParameters) -> c_int;
    fn vt_codec_set_thread_count(c: *mut AVCodecContext, n: c_int);
    fn vt_packet_stream_index(p: *const AVPacket) -> c_int;
    fn vt_frame_width(f: *const AVFrame) -> c_int;
    fn vt_frame_height(f: *const AVFrame) -> c_int;
    fn vt_frame_format(f: *const AVFrame) -> c_int;
    fn vt_frame_data(f: *const AVFrame) -> *const *mut u8;
    fn vt_frame_linesize(f: *const AVFrame) -> *const c_int;
    fn vt_avio_buffer(s: *mut AVIOContext) -> *mut u8;
}

type FnAvLogSetLevel = unsafe extern "C" fn(c_int);
type FnAvMalloc = unsafe extern "C" fn(usize) -> *mut c_void;
type FnAvFree = unsafe extern "C" fn(*mut c_void);
type FnAvFrameAlloc = unsafe extern "C" fn() -> *mut AVFrame;
type FnAvFrameFree = unsafe extern "C" fn(*mut *mut AVFrame);
type FnAvFrameUnref = unsafe extern "C" fn(*mut AVFrame);
type FnAvPacketAlloc = unsafe extern "C" fn() -> *mut AVPacket;
type FnAvPacketFree = unsafe extern "C" fn(*mut *mut AVPacket);
type FnAvPacketUnref = unsafe extern "C" fn(*mut AVPacket);
type FnAvcodecAllocContext3 = unsafe extern "C" fn(*const AVCodec) -> *mut AVCodecContext;
type FnAvcodecParametersToContext =
    unsafe extern "C" fn(*mut AVCodecContext, *const AVCodecParameters) -> c_int;
type FnAvcodecOpen2 =
    unsafe extern "C" fn(*mut AVCodecContext, *const AVCodec, *mut *mut AVDictionary) -> c_int;
type FnAvcodecSendPacket = unsafe extern "C" fn(*mut AVCodecContext, *const AVPacket) -> c_int;
type FnAvcodecReceiveFrame = unsafe extern "C" fn(*mut AVCodecContext, *mut AVFrame) -> c_int;
type FnAvcodecFlushBuffers = unsafe extern "C" fn(*mut AVCodecContext);
type FnAvcodecFreeContext = unsafe extern "C" fn(*mut *mut AVCodecContext);
type FnAvformatNetworkInit = unsafe extern "C" fn() -> c_int;
type FnAvformatAllocContext = unsafe extern "C" fn() -> *mut AVFormatContext;
type FnAvformatOpenInput = unsafe extern "C" fn(
    *mut *mut AVFormatContext,
    *const c_char,
    *const AVInputFormat,
    *mut *mut AVDictionary,
) -> c_int;
type FnAvformatFindStreamInfo =
    unsafe extern "C" fn(*mut AVFormatContext, *mut *mut AVDictionary) -> c_int;
type FnAvFindBestStream = unsafe extern "C" fn(
    *mut AVFormatContext,
    c_int,
    c_int,
    c_int,
    *mut *const AVCodec,
    c_int,
) -> c_int;
type FnAvReadFrame = unsafe extern "C" fn(*mut AVFormatContext, *mut AVPacket) -> c_int;
type FnAvSeekFrame = unsafe extern "C" fn(*mut AVFormatContext, c_int, i64, c_int) -> c_int;
type FnAvformatCloseInput = unsafe extern "C" fn(*mut *mut AVFormatContext);
type AvioReadPacket =
    Option<unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>;
type AvioWritePacket =
    Option<unsafe extern "C" fn(*mut c_void, *const u8, c_int) -> c_int>;
type AvioSeek =
    Option<unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64>;
type FnAvioAllocContext = unsafe extern "C" fn(
    *mut u8,
    c_int,
    c_int,
    *mut c_void,
    AvioReadPacket,
    AvioWritePacket,
    AvioSeek,
) -> *mut AVIOContext;
type FnAvioContextFree = unsafe extern "C" fn(*mut *mut AVIOContext);
type FnSwsGetContext = unsafe extern "C" fn(
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    c_int,
    *mut c_void,
    *mut c_void,
    *const f64,
) -> *mut SwsContext;
type FnSwsScale = unsafe extern "C" fn(
    *mut SwsContext,
    *const *const u8,
    *const c_int,
    c_int,
    c_int,
    *const *mut u8,
    *const c_int,
) -> c_int;
type FnSwsFreeContext = unsafe extern "C" fn(*mut SwsContext);

struct FfmpegApi {
    _mods: [HMODULE; 5],
    ready: bool,
    av_log_set_level: FnAvLogSetLevel,
    av_malloc: FnAvMalloc,
    av_free: FnAvFree,
    av_frame_alloc: FnAvFrameAlloc,
    av_frame_free: FnAvFrameFree,
    av_frame_unref: FnAvFrameUnref,
    av_packet_alloc: FnAvPacketAlloc,
    av_packet_free: FnAvPacketFree,
    av_packet_unref: FnAvPacketUnref,
    avcodec_alloc_context3: FnAvcodecAllocContext3,
    avcodec_parameters_to_context: FnAvcodecParametersToContext,
    avcodec_open2: FnAvcodecOpen2,
    avcodec_send_packet: FnAvcodecSendPacket,
    avcodec_receive_frame: FnAvcodecReceiveFrame,
    avcodec_flush_buffers: FnAvcodecFlushBuffers,
    avcodec_free_context: FnAvcodecFreeContext,
    avformat_network_init: FnAvformatNetworkInit,
    avformat_alloc_context: FnAvformatAllocContext,
    avformat_open_input: FnAvformatOpenInput,
    avformat_find_stream_info: FnAvformatFindStreamInfo,
    av_find_best_stream: FnAvFindBestStream,
    av_read_frame: FnAvReadFrame,
    av_seek_frame: FnAvSeekFrame,
    avformat_close_input: FnAvformatCloseInput,
    avio_alloc_context: FnAvioAllocContext,
    avio_context_free: FnAvioContextFree,
    sws_get_context: FnSwsGetContext,
    sws_scale: FnSwsScale,
    sws_free_context: FnSwsFreeContext,
}

unsafe impl Send for FfmpegApi {}
unsafe impl Sync for FfmpegApi {}

unsafe fn load_proc<T: Copy>(h: HMODULE, name: &'static [u8]) -> Option<T> {
    let p = GetProcAddress(h, name.as_ptr() as *const c_char);
    if p.is_null() {
        None
    } else {
        Some(mem::transmute_copy(&p))
    }
}

fn load_exact(stem: &str, major: c_int, optional: bool) -> Option<HMODULE> {
    let path = module_dir().join(format!("{stem}{major}.dll"));
    if !path.exists() {
        return if optional { Some(0) } else { None };
    }
    let wp = path_wide_z(&path);
    let h = unsafe {
        LoadLibraryExW(
            wp.as_ptr(),
            0,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    };
    if h == 0 {
        return None;
    }
    let got = module_path_from_handle(h);
    let expected = path.to_string_lossy();
    let same = got
        .as_ref()
        .map(|p| p.to_string_lossy().eq_ignore_ascii_case(&expected))
        .unwrap_or(false);
    if !same {
        unsafe {
            FreeLibrary(h);
        }
        return None;
    }
    Some(h)
}

fn ffmpeg_api() -> &'static FfmpegApi {
    static API: OnceLock<FfmpegApi> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let dummy: FnAvLogSetLevel = mem::transmute::<usize, FnAvLogSetLevel>(1usize);
        macro_rules! fail_api {
            () => {{
                return FfmpegApi {
                    _mods: [0; 5],
                    ready: false,
                    av_log_set_level: dummy,
                    av_malloc: mem::transmute(1usize),
                    av_free: mem::transmute(1usize),
                    av_frame_alloc: mem::transmute(1usize),
                    av_frame_free: mem::transmute(1usize),
                    av_frame_unref: mem::transmute(1usize),
                    av_packet_alloc: mem::transmute(1usize),
                    av_packet_free: mem::transmute(1usize),
                    av_packet_unref: mem::transmute(1usize),
                    avcodec_alloc_context3: mem::transmute(1usize),
                    avcodec_parameters_to_context: mem::transmute(1usize),
                    avcodec_open2: mem::transmute(1usize),
                    avcodec_send_packet: mem::transmute(1usize),
                    avcodec_receive_frame: mem::transmute(1usize),
                    avcodec_flush_buffers: mem::transmute(1usize),
                    avcodec_free_context: mem::transmute(1usize),
                    avformat_network_init: mem::transmute(1usize),
                    avformat_alloc_context: mem::transmute(1usize),
                    avformat_open_input: mem::transmute(1usize),
                    avformat_find_stream_info: mem::transmute(1usize),
                    av_find_best_stream: mem::transmute(1usize),
                    av_read_frame: mem::transmute(1usize),
                    av_seek_frame: mem::transmute(1usize),
                    avformat_close_input: mem::transmute(1usize),
                    avio_alloc_context: mem::transmute(1usize),
                    avio_context_free: mem::transmute(1usize),
                    sws_get_context: mem::transmute(1usize),
                    sws_scale: mem::transmute(1usize),
                    sws_free_context: mem::transmute(1usize),
                };
            }};
        }
        let avutil = match load_exact("avutil-", vt_libavutil_major(), false) {
            Some(h) if h != 0 => h,
            _ => fail_api!(),
        };
        let swresample = load_exact("swresample-", vt_libswresample_major(), true).unwrap_or(0);
        let avcodec = match load_exact("avcodec-", vt_libavcodec_major(), false) {
            Some(h) if h != 0 => h,
            _ => fail_api!(),
        };
        let avformat = match load_exact("avformat-", vt_libavformat_major(), false) {
            Some(h) if h != 0 => h,
            _ => fail_api!(),
        };
        let swscale = match load_exact("swscale-", vt_libswscale_major(), false) {
            Some(h) if h != 0 => h,
            _ => fail_api!(),
        };

        macro_rules! lp {
            ($h:expr, $n:literal, $t:ty) => {
                match load_proc::<$t>($h, concat!($n, "\0").as_bytes()) {
                    Some(f) => f,
                    None => fail_api!(),
                }
            };
        }
        let api = FfmpegApi {
            _mods: [avutil, swresample, avcodec, avformat, swscale],
            ready: true,
            av_log_set_level: lp!(avutil, "av_log_set_level", FnAvLogSetLevel),
            av_malloc: lp!(avutil, "av_malloc", FnAvMalloc),
            av_free: lp!(avutil, "av_free", FnAvFree),
            av_frame_alloc: lp!(avutil, "av_frame_alloc", FnAvFrameAlloc),
            av_frame_free: lp!(avutil, "av_frame_free", FnAvFrameFree),
            av_frame_unref: lp!(avutil, "av_frame_unref", FnAvFrameUnref),
            av_packet_alloc: lp!(avcodec, "av_packet_alloc", FnAvPacketAlloc),
            av_packet_free: lp!(avcodec, "av_packet_free", FnAvPacketFree),
            av_packet_unref: lp!(avcodec, "av_packet_unref", FnAvPacketUnref),
            avcodec_alloc_context3: lp!(avcodec, "avcodec_alloc_context3", FnAvcodecAllocContext3),
            avcodec_parameters_to_context: lp!(
                avcodec,
                "avcodec_parameters_to_context",
                FnAvcodecParametersToContext
            ),
            avcodec_open2: lp!(avcodec, "avcodec_open2", FnAvcodecOpen2),
            avcodec_send_packet: lp!(avcodec, "avcodec_send_packet", FnAvcodecSendPacket),
            avcodec_receive_frame: lp!(avcodec, "avcodec_receive_frame", FnAvcodecReceiveFrame),
            avcodec_flush_buffers: lp!(avcodec, "avcodec_flush_buffers", FnAvcodecFlushBuffers),
            avcodec_free_context: lp!(avcodec, "avcodec_free_context", FnAvcodecFreeContext),
            avformat_network_init: lp!(avformat, "avformat_network_init", FnAvformatNetworkInit),
            avformat_alloc_context: lp!(avformat, "avformat_alloc_context", FnAvformatAllocContext),
            avformat_open_input: lp!(avformat, "avformat_open_input", FnAvformatOpenInput),
            avformat_find_stream_info: lp!(
                avformat,
                "avformat_find_stream_info",
                FnAvformatFindStreamInfo
            ),
            av_find_best_stream: lp!(avformat, "av_find_best_stream", FnAvFindBestStream),
            av_read_frame: lp!(avformat, "av_read_frame", FnAvReadFrame),
            av_seek_frame: lp!(avformat, "av_seek_frame", FnAvSeekFrame),
            avformat_close_input: lp!(avformat, "avformat_close_input", FnAvformatCloseInput),
            avio_alloc_context: lp!(avformat, "avio_alloc_context", FnAvioAllocContext),
            avio_context_free: lp!(avformat, "avio_context_free", FnAvioContextFree),
            sws_get_context: lp!(swscale, "sws_getContext", FnSwsGetContext),
            sws_scale: lp!(swscale, "sws_scale", FnSwsScale),
            sws_free_context: lp!(swscale, "sws_freeContext", FnSwsFreeContext),
        };
        (api.av_log_set_level)(vt_av_log_quiet());
        (api.avformat_network_init)();
        api
    })
}

struct InterruptState {
    deadline: u64,
}
unsafe extern "C" fn ff_interrupt_cb(opaque: *mut c_void) -> c_int {
    if opaque.is_null() {
        return 0;
    }
    let s = &*(opaque as *const InterruptState);
    (GetTickCount64() >= s.deadline) as c_int
}

struct DecodeResources {
    api: &'static FfmpegApi,
    fmt: *mut AVFormatContext,
    dec: *mut AVCodecContext,
    pkt: *mut AVPacket,
    frame: *mut AVFrame,
    avio: *mut AVIOContext,
}
impl Drop for DecodeResources {
    fn drop(&mut self) {
        unsafe {
            if !self.frame.is_null() {
                (self.api.av_frame_free)(&mut self.frame);
            }
            if !self.pkt.is_null() {
                (self.api.av_packet_free)(&mut self.pkt);
            }
            if !self.dec.is_null() {
                (self.api.avcodec_free_context)(&mut self.dec);
            }
            if !self.fmt.is_null() {
                (self.api.avformat_close_input)(&mut self.fmt);
            }
            if !self.avio.is_null() {
                // FFmpeg may replace AVIOContext::buffer while probing.
                // Read the current pointer after avformat_close_input(), just
                // like FFmpeg's official custom-AVIO example, then free it
                // before freeing the AVIOContext itself.
                let buffer = vt_avio_buffer(self.avio);
                if !buffer.is_null() {
                    (self.api.av_free)(buffer as *mut c_void);
                }
                (self.api.avio_context_free)(&mut self.avio);
            }
        }
    }
}

unsafe fn receive_one_frame(
    api: &FfmpegApi,
    dec: *mut AVCodecContext,
    frame: *mut AVFrame,
) -> bool {
    (api.avcodec_receive_frame)(dec, frame) == 0
}

unsafe fn decode_frame_from_current(
    api: &FfmpegApi,
    fmt: *mut AVFormatContext,
    dec: *mut AVCodecContext,
    video_index: c_int,
    pkt: *mut AVPacket,
    frame: *mut AVFrame,
    packet_limit: i32,
) -> bool {
    // Hoisted out of the loop: these cross into the C shim (a separate
    // translation unit the Rust-side `lto = "fat"` can't inline through),
    // and the value never changes for the life of the call, so fetching it
    // once avoids up to `packet_limit` redundant cross-TU calls.
    let eagain = vt_averror_eagain();
    let eof = vt_averror_eof();
    let mut packets = 0;
    while packets < packet_limit && (api.av_read_frame)(fmt, pkt) >= 0 {
        packets += 1;
        if vt_packet_stream_index(pkt) != video_index {
            (api.av_packet_unref)(pkt);
            continue;
        }
        let mut s = (api.avcodec_send_packet)(dec, pkt);
        if s == eagain {
            if receive_one_frame(api, dec, frame) {
                (api.av_packet_unref)(pkt);
                return true;
            }
            s = (api.avcodec_send_packet)(dec, pkt);
        }
        (api.av_packet_unref)(pkt);
        if s < 0 {
            continue;
        }
        let r = (api.avcodec_receive_frame)(dec, frame);
        if r == 0 {
            return true;
        }
        if r != eagain && r != eof {
            continue;
        }
    }
    (api.avcodec_send_packet)(dec, null());
    receive_one_frame(api, dec, frame)
}

unsafe fn scale_frame_to_thumb(
    api: &FfmpegApi,
    frame: *const AVFrame,
    max_w: i32,
    max_h: i32,
) -> Option<Arc<ThumbData>> {
    let fw = vt_frame_width(frame);
    let fh = vt_frame_height(frame);
    let ff = vt_frame_format(frame);
    if fw <= 0 || fh <= 0 || ff < 0 || max_w <= 0 || max_h <= 0 {
        return None;
    }
    let scale = (max_w as f64 / fw as f64).min(max_h as f64 / fh as f64);
    let out_w = ((fw as f64 * scale + 0.5).floor() as i32).max(1);
    let out_h = ((fh as f64 * scale + 0.5).floor() as i32).max(1);
    let stride = out_w.checked_mul(4)?;
    let len = (stride as usize).checked_mul(out_h as usize)?;
    let mut bgra = vec![0u8; len];
    let sws = (api.sws_get_context)(
        fw,
        fh,
        ff,
        out_w,
        out_h,
        vt_av_pix_fmt_bgra(),
        vt_sws_fast_bilinear(),
        null_mut(),
        null_mut(),
        null(),
    );
    if sws.is_null() {
        return None;
    }
    let src_data = vt_frame_data(frame) as *const *const u8;
    let src_lines = vt_frame_linesize(frame);
    let dst0 = bgra.as_mut_ptr();
    let dst_data = [dst0, null_mut(), null_mut(), null_mut()];
    let dst_lines = [stride, 0, 0, 0];
    let rows = (api.sws_scale)(
        sws,
        src_data,
        src_lines,
        0,
        fh,
        dst_data.as_ptr(),
        dst_lines.as_ptr(),
    );
    (api.sws_free_context)(sws);
    if rows <= 0 {
        return None;
    }
    Some(Arc::new(ThumbData {
        width: out_w,
        height: out_h,
        stride,
        bgra,
    }))
}

// ----------------------- FFmpeg memory-image fallback ----------------------

struct MemoryIo {
    data: *const u8,
    len: usize,
    pos: usize,
}

unsafe extern "C" fn memory_read_packet(
    opaque: *mut c_void,
    buf: *mut u8,
    buf_size: c_int,
) -> c_int {
    if opaque.is_null() || buf.is_null() || buf_size < 0 {
        return vt_averror_einval();
    }
    let state = &mut *(opaque as *mut MemoryIo);
    let remaining = state.len.saturating_sub(state.pos);
    if remaining == 0 || buf_size == 0 {
        return vt_averror_eof();
    }

    let n = remaining.min(buf_size as usize);
    ptr::copy_nonoverlapping(state.data.add(state.pos), buf, n);
    state.pos += n;
    n as c_int
}

unsafe extern "C" fn memory_seek(
    opaque: *mut c_void,
    offset: i64,
    whence: c_int,
) -> i64 {
    if opaque.is_null() {
        return vt_averror_einval() as i64;
    }
    let state = &mut *(opaque as *mut MemoryIo);

    let size_flag = vt_avseek_size();
    if (whence & size_flag) != 0 {
        return state.len.min(i64::MAX as usize) as i64;
    }

    let base_whence = whence & !(size_flag | vt_avseek_force());
    let base = match base_whence {
        0 => 0i128,             // SEEK_SET
        1 => state.pos as i128, // SEEK_CUR
        2 => state.len as i128, // SEEK_END
        _ => return vt_averror_einval() as i64,
    };
    let new_pos = base + offset as i128;
    if new_pos < 0 || new_pos > usize::MAX as i128 || new_pos > i64::MAX as i128 {
        return vt_averror_einval() as i64;
    }

    state.pos = new_pos as usize;
    new_pos as i64
}

fn source_pixels_allowed(w: i32, h: i32) -> bool {
    if w <= 0 || h <= 0 {
        return false;
    }
    let pixels = (w as u64).saturating_mul(h as u64);
    pixels <= settings().zip_max_source_mp as u64 * 1_000_000
}

fn decode_image_ffmpeg_memory(
    bytes: &[u8],
    max_w: i32,
    max_h: i32,
) -> Option<Arc<ThumbData>> {
    if bytes.is_empty() || max_w <= 0 || max_h <= 0 {
        return None;
    }

    let api = ffmpeg_api();
    if !api.ready {
        return None;
    }

    // The Box allocation keeps the opaque state at a stable address while
    // libavformat calls back into Rust.
    let mut io = Box::new(MemoryIo {
        data: bytes.as_ptr(),
        len: bytes.len(),
        pos: 0,
    });
    let interrupt = Box::new(InterruptState {
        deadline: unsafe { GetTickCount64() } + settings().timeout_ms as u64,
    });

    let mut r = DecodeResources {
        api,
        fmt: unsafe { (api.avformat_alloc_context)() },
        dec: null_mut(),
        pkt: null_mut(),
        frame: null_mut(),
        avio: null_mut(),
    };
    if r.fmt.is_null() {
        return None;
    }

    const AVIO_BUFFER_SIZE: c_int = 32 * 1024;
    let avio_buffer = unsafe { (api.av_malloc)(AVIO_BUFFER_SIZE as usize) } as *mut u8;
    if avio_buffer.is_null() {
        return None;
    }

    r.avio = unsafe {
        (api.avio_alloc_context)(
            avio_buffer,
            AVIO_BUFFER_SIZE,
            0,
            (&mut *io as *mut MemoryIo).cast::<c_void>(),
            Some(memory_read_packet),
            None,
            Some(memory_seek),
        )
    };
    if r.avio.is_null() {
        unsafe {
            (api.av_free)(avio_buffer as *mut c_void);
        }
        return None;
    }

    unsafe {
        vt_format_set_pb(r.fmt, r.avio);
        vt_format_set_interrupt(
            r.fmt,
            Some(ff_interrupt_cb),
            (&*interrupt as *const InterruptState) as *mut c_void,
        );
    }

    // A NULL URL tells libavformat to probe the custom AVIOContext instead
    // of opening a file. This keeps ZIP entries entirely in memory.
    if unsafe { (api.avformat_open_input)(&mut r.fmt, null(), null(), null_mut()) } < 0
        || r.fmt.is_null()
    {
        return None;
    }
    if unsafe { (api.avformat_find_stream_info)(r.fmt, null_mut()) } < 0 {
        return None;
    }

    let mut decoder: *const AVCodec = null();
    let vi = unsafe {
        (api.av_find_best_stream)(
            r.fmt,
            vt_avmedia_type_video(),
            -1,
            -1,
            &mut decoder,
            0,
        )
    };
    if vi < 0 || decoder.is_null() || vi as u32 >= unsafe { vt_format_nb_streams(r.fmt) } {
        return None;
    }

    let stream = unsafe { vt_format_stream(r.fmt, vi as u32) };
    let codecpar = unsafe { vt_stream_codecpar(stream) };
    if codecpar.is_null() {
        return None;
    }

    // Reject oversized still images before allocating decoder output whenever
    // the demuxer already knows the dimensions.
    let par_w = unsafe { vt_codecpar_width(codecpar) };
    let par_h = unsafe { vt_codecpar_height(codecpar) };
    if par_w > 0 && par_h > 0 && !source_pixels_allowed(par_w, par_h) {
        return None;
    }

    r.dec = unsafe { (api.avcodec_alloc_context3)(decoder) };
    if r.dec.is_null() {
        return None;
    }
    if unsafe { (api.avcodec_parameters_to_context)(r.dec, codecpar) } < 0 {
        return None;
    }
    if settings().decoder_threads > 0 {
        unsafe {
            vt_codec_set_thread_count(r.dec, settings().decoder_threads);
        }
    }
    if unsafe { (api.avcodec_open2)(r.dec, decoder, null_mut()) } < 0 {
        return None;
    }

    r.pkt = unsafe { (api.av_packet_alloc)() };
    r.frame = unsafe { (api.av_frame_alloc)() };
    if r.pkt.is_null() || r.frame.is_null() {
        return None;
    }

    if !unsafe {
        decode_frame_from_current(
            api,
            r.fmt,
            r.dec,
            vi,
            r.pkt,
            r.frame,
            settings().packet_limit,
        )
    } {
        return None;
    }

    let fw = unsafe { vt_frame_width(r.frame) };
    let fh = unsafe { vt_frame_height(r.frame) };
    if !source_pixels_allowed(fw, fh) {
        return None;
    }

    unsafe { scale_frame_to_thumb(api, r.frame, max_w, max_h) }
}

// -------------------------- smart frame analysis ---------------------------

#[derive(Default, Clone, Copy)]
struct FrameStats {
    dark_ratio: f64,
    mid_ratio: f64,
    mean_luma: f64,
    luma_stddev: f64,
    color_stddev: f64,
    dominant_color_ratio: f64,
    edge_ratio: f64,
    mean_gradient: f64,
}

fn pixel_luma(p: &[u8]) -> i32 {
    (29 * p[0] as i32 + 150 * p[1] as i32 + 77 * p[2] as i32 + 128) >> 8
}

fn frame_stats(t: &ThumbData, cfg: &Settings) -> FrameStats {
    if t.width <= 0 || t.height <= 0 || t.stride < t.width * 4 || t.bgra.is_empty() {
        return FrameStats::default();
    }
    let step: usize = 4;
    const BRIGHT: i32 = 180;
    let width = t.width as usize;
    let height = t.height as usize;
    let stride = t.stride as usize;
    let sampled_cols = width.div_ceil(step);

    let mut dark = 0u64;
    let mut mid = 0u64;
    let mut total = 0u64;
    let mut luma_sum = 0u64;
    let mut luma_sq = 0u64;
    let mut b_sum = 0u64;
    let mut g_sum = 0u64;
    let mut r_sum = 0u64;
    let mut b_sq = 0u64;
    let mut g_sq = 0u64;
    let mut r_sq = 0u64;
    let mut edge_count = 0u64;
    let mut grad_count = 0u64;
    let mut grad_sum = 0u64;
    let mut bins = [0u32; 512];

    // One byte per sampled column is enough because luma is always 0..=255.
    // The buffer stores the previous sampled row. While scanning the current
    // row we first compare against the old value (vertical gradient), then
    // overwrite that slot with the current luma. Horizontal gradients only
    // need the immediately preceding luma, kept in `left_luma`. This preserves
    // Claude's "compute every luma once" optimization while reducing two
    // Vec<i32> allocations/buffers to one compact Vec<u8>.
    let mut prev_luma = vec![0u8; sampled_cols];
    let mut have_prev = false;

    let mut y = 0usize;
    while y < height {
        let row = &t.bgra[y * stride..y * stride + stride];
        let mut left_luma: Option<u8> = None;
        let mut col = 0usize;
        let mut x = 0usize;
        while x < width {
            let p = &row[x * 4..x * 4 + 4];
            let b = p[0] as i32;
            let g = p[1] as i32;
            let r = p[2] as i32;
            let l = pixel_luma(p);
            let l8 = l as u8;
            if l <= cfg.black_threshold {
                dark += 1;
            } else if l < BRIGHT {
                mid += 1;
            }
            luma_sum += l as u64;
            luma_sq += (l * l) as u64;
            b_sum += b as u64;
            g_sum += g as u64;
            r_sum += r as u64;
            b_sq += (b * b) as u64;
            g_sq += (g * g) as u64;
            r_sq += (r * r) as u64;
            let bin = (((r as usize) >> 5) << 6) | (((g as usize) >> 5) << 3) | ((b as usize) >> 5);
            bins[bin] += 1;
            total += 1;

            if let Some(left) = left_luma {
                let d = (l8 as i32 - left as i32).unsigned_abs() as u64;
                grad_sum += d;
                grad_count += 1;
                if d >= cfg.edge_threshold as u64 {
                    edge_count += 1;
                }
            }
            left_luma = Some(l8);

            if have_prev {
                let d = (l8 as i32 - prev_luma[col] as i32).unsigned_abs() as u64;
                grad_sum += d;
                grad_count += 1;
                if d >= cfg.edge_threshold as u64 {
                    edge_count += 1;
                }
            }
            prev_luma[col] = l8;
            col += 1;
            x += step;
        }
        have_prev = true;
        y += step;
    }

    if total == 0 {
        return FrameStats::default();
    }
    let dominant = bins.iter().copied().max().unwrap_or(0);
    let n = total as f64;
    let stddev = |sum: u64, sq: u64| {
        let m = sum as f64 / n;
        (sq as f64 / n - m * m).max(0.0).sqrt()
    };
    let bsd = stddev(b_sum, b_sq);
    let gsd = stddev(g_sum, g_sq);
    let rsd = stddev(r_sum, r_sq);
    FrameStats {
        dark_ratio: dark as f64 / n,
        mid_ratio: mid as f64 / n,
        mean_luma: luma_sum as f64 / n,
        luma_stddev: stddev(luma_sum, luma_sq),
        color_stddev: ((bsd * bsd + gsd * gsd + rsd * rsd) / 3.0).sqrt(),
        dominant_color_ratio: dominant as f64 / n,
        edge_ratio: if grad_count > 0 {
            edge_count as f64 / grad_count as f64
        } else {
            0.0
        },
        mean_gradient: if grad_count > 0 {
            grad_sum as f64 / grad_count as f64
        } else {
            0.0
        },
    }
}

fn frame_analysis_enabled(c: &Settings) -> bool {
    c.black_frame_detection
        || c.title_card_detection
        || c.uniform_frame_detection
        || c.transition_detection
}
fn suspicious_frame(st: &FrameStats, c: &Settings) -> bool {
    if c.black_frame_detection && st.dark_ratio >= c.black_pixel_percent as f64 / 100.0 {
        return true;
    }
    if c.title_card_detection
        && st.dark_ratio >= c.title_card_dark_percent as f64 / 100.0
        && st.mid_ratio <= c.title_card_midtone_max_percent as f64 / 100.0
        && st.mean_luma <= c.title_card_mean_luma_max as f64
    {
        return true;
    }
    if c.uniform_frame_detection {
        if st.dominant_color_ratio >= c.dominant_color_percent as f64 / 100.0 {
            return true;
        }
        if st.luma_stddev <= c.max_luma_stddev as f64
            && st.color_stddev <= c.max_color_stddev as f64
        {
            return true;
        }
    }
    if c.transition_detection
        && st.edge_ratio < c.min_edge_percent as f64 / 100.0
        && st.mean_gradient <= c.max_mean_gradient as f64
    {
        return true;
    }
    false
}
fn frame_quality_score(st: &FrameStats) -> f64 {
    st.edge_ratio * 2200.0
        + st.luma_stddev * 9.0
        + st.color_stddev * 5.0
        + st.mid_ratio * 180.0
        + (1.0 - st.dominant_color_ratio) * 420.0
        + st.mean_gradient.min(40.0) * 4.0
        - st.dark_ratio * 120.0
}

// -------------------------- ZIP image sequences ----------------------------

fn has_zip_signature(buf: &[u8]) -> bool {
    buf.len() >= 4
        && buf[0] == b'P'
        && buf[1] == b'K'
        && matches!((buf[2], buf[3]), (3, 4) | (5, 6) | (7, 8))
}
fn is_image_entry_name(name: &str) -> bool {
    let ext = Path::new(name)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "bmp" | "gif" | "tif" | "tiff" | "webp" | "heic" | "heif"
    )
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let aa = a.as_bytes();
    let bb = b.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);
    while i < aa.len() && j < bb.len() {
        if aa[i].is_ascii_digit() && bb[j].is_ascii_digit() {
            let (za, zb) = (i, j);
            while i < aa.len() && aa[i] == b'0' {
                i += 1
            }
            while j < bb.len() && bb[j] == b'0' {
                j += 1
            }
            let (na, nb) = (i, j);
            while i < aa.len() && aa[i].is_ascii_digit() {
                i += 1
            }
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1
            }
            let (la, lb) = (i - na, j - nb);
            if la != lb {
                return la.cmp(&lb);
            }
            let c = aa[na..i].cmp(&bb[nb..j]);
            if c != Ordering::Equal {
                return c;
            }
            let zc = (na - za).cmp(&(nb - zb));
            if zc != Ordering::Equal {
                return zc;
            }
            continue;
        }
        let ca = aa[i].to_ascii_lowercase();
        let cb = bb[j].to_ascii_lowercase();
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
        j += 1;
    }
    aa.len().cmp(&bb.len())
}

fn decode_image_rust(bytes: &[u8], max_w: i32, max_h: i32) -> Option<Arc<ThumbData>> {
    if bytes.is_empty() || max_w <= 0 || max_h <= 0 {
        return None;
    }
    // A single decoder handles both steps below. Format-guessing and header
    // parsing used to happen twice: once via into_dimensions() just to
    // gate oversized images, then again from scratch in load_from_memory()
    // to actually decode. into_decoder() parses the header once; reusing
    // that same decoder in DynamicImage::from_decoder() finishes the job
    // instead of starting over.
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let decoder = reader.into_decoder().ok()?;
    let (sw, sh) = decoder.dimensions();
    if sw == 0 || sh == 0 {
        return None;
    }
    if sw as u64 * sh as u64 > settings().zip_max_source_mp as u64 * 1_000_000 {
        return None;
    }
    let img = DynamicImage::from_decoder(decoder).ok()?;
    let scale = (max_w as f64 / sw as f64).min(max_h as f64 / sh as f64);
    let ow = ((sw as f64 * scale + 0.5).floor() as u32).max(1);
    let oh = ((sh as f64 * scale + 0.5).floor() as u32).max(1);
    let resized = if ow != sw || oh != sh {
        img.resize_exact(ow, oh, FilterType::Triangle)
    } else {
        img
    };
    let rgba = resized.to_rgba8();
    let mut bgra = rgba.into_raw();
    for p in bgra.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    Some(Arc::new(ThumbData {
        width: ow as i32,
        height: oh as i32,
        stride: ow as i32 * 4,
        bgra,
    }))
}

fn decode_image(bytes: &[u8], max_w: i32, max_h: i32) -> Option<Arc<ThumbData>> {
    // Fast/native Rust path first for the common formats. If the `image`
    // crate does not recognize or decode the entry (notably HEIC/HEIF),
    // feed the same in-memory bytes to the already bundled FFmpeg through a
    // custom AVIOContext. No temp file and no Windows-installed codec needed.
    decode_image_rust(bytes, max_w, max_h)
        .or_else(|| decode_image_ffmpeg_memory(bytes, max_w, max_h))
}

struct ZipCandidate {
    index: usize,
    name: String,
    size: u64,
}

impl PartialEq for ZipCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && natural_cmp(&self.name, &other.name) == Ordering::Equal
    }
}
impl Eq for ZipCandidate {}
impl PartialOrd for ZipCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ZipCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Natural filename order, then archive position as the stable-sort
        // tie-breaker. BinaryHeap is a max-heap, so peek() is the worst of
        // the currently retained candidates.
        natural_cmp(&self.name, &other.name).then_with(|| self.index.cmp(&other.index))
    }
}

fn decode_zip_first_image(path: &Path, max_w: i32, max_h: i32) -> Option<Arc<ThumbData>> {
    if !settings().zip_sequences {
        return None;
    }
    let file = File::open(path).ok()?;
    let mut zip = ZipArchive::new(file).ok()?;
    let max_entry = settings().zip_max_entry_mb as u64 * 1024 * 1024;
    let keep = settings().zip_max_candidates as usize;
    if keep == 0 {
        return None;
    }

    // Keep only the best K image entries while scanning the archive. This
    // preserves the original stable natural-order semantics without letting
    // a hostile/pathological ZIP make candidate memory grow with zip.len().
    // Complexity: O(entries * log K) time and O(K) retained memory, where K
    // is MaxCandidates (default 16, hard-clamped to at most 128).
    let mut cands: BinaryHeap<ZipCandidate> = BinaryHeap::with_capacity(keep);
    for i in 0..zip.len() {
        let f = zip.by_index(i).ok()?;
        let size = f.size();
        if f.is_dir() || size == 0 || size > max_entry || !is_image_entry_name(f.name()) {
            continue;
        }

        let should_keep = if cands.len() < keep {
            true
        } else if let Some(worst) = cands.peek() {
            match natural_cmp(f.name(), &worst.name) {
                Ordering::Less => true,
                Ordering::Equal => i < worst.index,
                Ordering::Greater => false,
            }
        } else {
            true
        };

        if should_keep {
            if cands.len() == keep {
                cands.pop();
            }
            cands.push(ZipCandidate {
                index: i,
                name: f.name().to_owned(),
                size,
            });
        }
    }

    let mut cands = cands.into_vec();
    cands.sort_by(|a, b| a.cmp(b));
    for cand in cands {
        let f = zip.by_index(cand.index).ok()?;
        let mut bytes = Vec::with_capacity(cand.size as usize);
        let mut limited = f.take(max_entry + 1);
        limited.read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > max_entry {
            continue;
        }
        if let Some(t) = decode_image(&bytes, max_w, max_h) {
            return Some(t);
        }
    }
    None
}

// ----------------------------- video decode --------------------------------

fn wide_path(ptr: *const u16) -> Option<(Vec<u16>, PathBuf)> {
    if ptr.is_null() {
        return None;
    }
    let mut n = 0usize;
    unsafe {
        while *ptr.add(n) != 0 {
            n += 1;
            if n > 32767 {
                return None;
            }
        }
    }
    let w = unsafe { slice::from_raw_parts(ptr, n) }.to_vec();
    let p = PathBuf::from(OsString::from_wide(&w));
    Some((w, p))
}

fn utf8_from_wide(w: &[u16]) -> Option<CString> {
    CString::new(String::from_utf16(w).ok()?).ok()
}

fn resolved_final_path(path: &Path) -> Option<PathBuf> {
    let p = std::fs::canonicalize(path).ok()?;
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        Some(PathBuf::from(format!(r"\\{}", rest)))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        Some(PathBuf::from(rest))
    } else {
        Some(p)
    }
}

fn decode_with_libav_utf8(input: &CString, max_w: i32, max_h: i32) -> Option<Arc<ThumbData>> {
    let api = ffmpeg_api();
    if !api.ready {
        return None;
    }
    let interrupt = Box::new(InterruptState {
        deadline: unsafe { GetTickCount64() } + settings().timeout_ms as u64,
    });
    let mut r = DecodeResources {
        api,
        fmt: unsafe { (api.avformat_alloc_context)() },
        dec: null_mut(),
        pkt: null_mut(),
        frame: null_mut(),
        avio: null_mut(),
    };
    if r.fmt.is_null() {
        return None;
    }
    unsafe {
        vt_format_set_interrupt(
            r.fmt,
            Some(ff_interrupt_cb),
            (&*interrupt as *const InterruptState) as *mut c_void,
        );
    }
    if unsafe { (api.avformat_open_input)(&mut r.fmt, input.as_ptr(), null(), null_mut()) } < 0
        || r.fmt.is_null()
    {
        return None;
    }
    if unsafe { (api.avformat_find_stream_info)(r.fmt, null_mut()) } < 0 {
        return None;
    }
    let mut decoder: *const AVCodec = null();
    let vi = unsafe {
        (api.av_find_best_stream)(r.fmt, vt_avmedia_type_video(), -1, -1, &mut decoder, 0)
    };
    if vi < 0 || decoder.is_null() || vi as u32 >= unsafe { vt_format_nb_streams(r.fmt) } {
        return None;
    }
    r.dec = unsafe { (api.avcodec_alloc_context3)(decoder) };
    if r.dec.is_null() {
        return None;
    }
    let stream = unsafe { vt_format_stream(r.fmt, vi as u32) };
    let codecpar = unsafe { vt_stream_codecpar(stream) };
    if codecpar.is_null() {
        return None;
    }
    if unsafe { (api.avcodec_parameters_to_context)(r.dec, codecpar) } < 0 {
        return None;
    }
    if settings().decoder_threads > 0 {
        unsafe {
            vt_codec_set_thread_count(r.dec, settings().decoder_threads);
        }
    }
    if unsafe { (api.avcodec_open2)(r.dec, decoder, null_mut()) } < 0 {
        return None;
    }
    r.pkt = unsafe { (api.av_packet_alloc)() };
    r.frame = unsafe { (api.av_frame_alloc)() };
    if r.pkt.is_null() || r.frame.is_null() {
        return None;
    }
    let cfg = settings();
    let duration = unsafe { vt_format_duration(r.fmt) };
    let mut targets = Vec::<i64>::new();
    let mut add = |t: i64| {
        if t < 0 {
            return;
        }
        if duration > 0 && t >= duration {
            return;
        }
        if targets.iter().any(|&o| (o - t).abs() < 100_000) {
            return;
        }
        targets.push(t);
    };
    let mut first = cfg.seek_ms as i64 * 1000;
    if duration > 0 && first >= duration {
        first = duration / 3;
    }
    add(first.max(0));
    let analyze = frame_analysis_enabled(cfg);
    if analyze {
        for &ms in &cfg.fallback_seek_ms {
            add(ms as i64 * 1000);
        }
        if duration > 0 && cfg.fallback_percent > 0 {
            add(duration * cfg.fallback_percent as i64 / 100);
        }
    }
    let mut best: Option<Arc<ThumbData>> = None;
    let mut best_q = -1.0f64;
    let mut result = None;
    let mut all_targets = targets;
    if !all_targets.contains(&0) {
        all_targets.push(0);
    }
    // Fetched once instead of once per seek target: it's the same constant
    // every time. This loop only runs a handful of iterations (bounded by
    // the fallback-seek list), so the win is small, but it's free while
    // already touching this loop for the frame_stats optimization below.
    let seek_backward = unsafe { vt_avseek_flag_backward() };
    for target in all_targets {
        if unsafe { GetTickCount64() } >= interrupt.deadline {
            break;
        }
        unsafe {
            (api.av_frame_unref)(r.frame);
            (api.avcodec_flush_buffers)(r.dec);
        }
        if unsafe { (api.av_seek_frame)(r.fmt, -1, target, seek_backward) } < 0 {
            continue;
        }
        if !unsafe {
            decode_frame_from_current(api, r.fmt, r.dec, vi, r.pkt, r.frame, cfg.packet_limit)
        } {
            continue;
        }
        let Some(thumb) = (unsafe { scale_frame_to_thumb(api, r.frame, max_w, max_h) }) else {
            continue;
        };
        if !analyze {
            result = Some(thumb);
            break;
        }
        let st = frame_stats(&thumb, cfg);
        if !suspicious_frame(&st, cfg) {
            result = Some(thumb);
            break;
        }
        let q = frame_quality_score(&st);
        if best.is_none() || q > best_q {
            best = Some(thumb);
            best_q = q;
        }
    }
    result.or(best)
}

fn decode_path(
    wide: &[u16],
    path: &Path,
    max_w: i32,
    max_h: i32,
    content: &[u8],
) -> Option<Arc<ThumbData>> {
    let known = content.len() >= 4;
    let zip_hint = has_zip_signature(content);
    if zip_hint {
        if let Some(t) = decode_zip_first_image(path, max_w, max_h) {
            return Some(t);
        }
    }
    if let Some(input) = utf8_from_wide(wide) {
        if let Some(t) = decode_with_libav_utf8(&input, max_w, max_h) {
            return Some(t);
        }
    }
    if !zip_hint && !known {
        if let Some(t) = decode_zip_first_image(path, max_w, max_h) {
            return Some(t);
        }
    }
    if settings().resolve_symlink_fallback {
        if let Some(final_path) = resolved_final_path(path) {
            if final_path != path {
                let fw: Vec<u16> = final_path.as_os_str().encode_wide().collect();
                if let Some(input) = utf8_from_wide(&fw) {
                    if let Some(t) = decode_with_libav_utf8(&input, max_w, max_h) {
                        return Some(t);
                    }
                }
                if settings().zip_sequences {
                    if let Some(t) = decode_zip_first_image(&final_path, max_w, max_h) {
                        return Some(t);
                    }
                }
            }
        }
    }
    None
}

fn make_thumb(
    file: *const u16,
    max_w: i32,
    max_h: i32,
    content: *const c_char,
    content_len: i32,
) -> HBITMAP {
    if file.is_null() || max_w <= 0 || max_h <= 0 || max_w > 8192 || max_h > 8192 {
        return 0;
    }
    let Some((wide, path)) = wide_path(file) else {
        return 0;
    };
    let key = CacheKey {
        path: normalized_cache_path(&wide),
        max_w,
        max_h,
    };
    if let Some(hit) = cache().lock().ok().and_then(|mut c| c.get(&key)) {
        return make_hbitmap(&hit);
    }
    let permit = DecodePermit::acquire();
    if !permit.held {
        return 0;
    }
    if let Some(hit) = cache().lock().ok().and_then(|mut c| c.get(&key)) {
        return make_hbitmap(&hit);
    }
    let content_slice = if content.is_null() || content_len <= 0 {
        &[][..]
    } else {
        unsafe { slice::from_raw_parts(content as *const u8, content_len as usize) }
    };
    let Some(data) = decode_path(&wide, &path, max_w, max_h, content_slice) else {
        return 0;
    };
    if let Ok(mut c) = cache().lock() {
        c.put(key, data.clone());
    }
    make_hbitmap(&data)
}

// ------------------------------- WLX API -----------------------------------

#[no_mangle]
pub extern "system" fn ListLoad(_parent: HWND, _file: *mut c_char, _flags: c_int) -> HWND {
    0
}
#[no_mangle]
pub extern "system" fn ListLoadW(_parent: HWND, _file: *mut u16, _flags: c_int) -> HWND {
    0
}

#[no_mangle]
pub extern "system" fn ListGetDetectString(out: *mut c_char, maxlen: c_int) {
    const S:&[u8]=b"EXT=\"MP4\" | EXT=\"MKV\" | EXT=\"AVI\" | EXT=\"MOV\" | EXT=\"WEBM\" | EXT=\"WMV\" | EXT=\"M4V\" | EXT=\"MPG\" | EXT=\"MPEG\" | EXT=\"TS\" | EXT=\"M2TS\" | EXT=\"FLV\" | EXT=\"VOB\" | EXT=\"3GP\" | EXT=\"OGV\" | EXT=\"ASF\" | EXT=\"RM\" | EXT=\"RMVB\" | EXT=\"F4V\"\0";
    if out.is_null() || maxlen <= 0 {
        return;
    }
    let n = (maxlen as usize - 1).min(S.len() - 1);
    unsafe {
        ptr::copy_nonoverlapping(S.as_ptr(), out as *mut u8, n);
        *out.add(n) = 0;
    }
}

#[no_mangle]
pub extern "system" fn ListGetPreviewBitmapW(
    file: *mut u16,
    width: c_int,
    height: c_int,
    content: *mut c_char,
    content_len: c_int,
) -> HBITMAP {
    catch_unwind(AssertUnwindSafe(|| {
        make_thumb(file, width, height, content, content_len)
    }))
    .unwrap_or(0)
}

#[no_mangle]
pub extern "system" fn ListGetPreviewBitmap(
    _file: *mut c_char,
    _width: c_int,
    _height: c_int,
    _content: *mut c_char,
    _content_len: c_int,
) -> HBITMAP {
    0
}
