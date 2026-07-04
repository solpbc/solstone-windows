// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Windows platform glue.
//!
//! **Platform tier** — `windows-rs` quarantine; `unsafe` permitted here only.
//! Holds the OS-bound seams the engine and shell need: the session/power
//! notification pump, the per-session named-mutex single-instance gate, the
//! `%LocalAppData%` path layout, and the real `SegmentFs` / `RecoveryFs`
//! implementations that back the pure rotation and recovery logic.
//!
//! The filesystem implementations are std-only and host-testable. The
//! notification pump and named-mutex gate use Windows APIs only behind
//! `#[cfg(windows)]`; off-Windows they remain honest no-op stubs so the platform
//! crate compiles on the Linux dev host.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use observer_model::{
    AudioFormat, CaptureChunk, SegmentKey, SourceKind, AUDIO_FILE_NAME, LEN_FILE_NAME,
    SCREEN_FILE_NAME,
};
use observer_recovery::{RecoveryFs, StaleSegment};
use observer_segment::{is_live_segment, SegmentFs, DEFAULT_SEGMENT_SECS};

pub mod autostart;
pub mod local_offset;

pub use local_offset::WindowsLocalOffset;

/// The per-user data root: `%LocalAppData%\Solstone`. Falls back to a temp path
/// off-Windows so the type is host-constructible for tests.
pub fn local_data_root() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let mut p = PathBuf::from(local);
        p.push("Solstone");
        p
    } else {
        // Host fallback (dev/test). Production always has %LocalAppData%.
        let mut p = std::env::temp_dir();
        p.push("solstone");
        p
    }
}

/// The active segments directory under the data root.
pub fn segments_dir() -> PathBuf {
    local_data_root().join("segments")
}

/// The log directory under the data root (`make run` tails this).
pub fn logs_dir() -> PathBuf {
    local_data_root().join("logs")
}

fn incomplete_dir(root: &Path, key: SegmentKey) -> PathBuf {
    root.join(format!("{}.incomplete", key.index))
}

fn sealed_dir(root: &Path, key: SegmentKey) -> PathBuf {
    root.join(key.index.to_string())
}

fn source_file_name(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Screen => SCREEN_FILE_NAME,
        SourceKind::SystemAudio => "system-audio.pcm",
        SourceKind::Mic => "mic.pcm",
    }
}

fn source_fmt_file_name(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Screen => unreachable!("screen chunks do not have audio format sidecars"),
        SourceKind::SystemAudio => "system-audio.fmt.json",
        SourceKind::Mic => "mic.fmt.json",
    }
}

fn has_sealable_media(dir: &Path) -> io::Result<bool> {
    // Partial files are never counted as sealable media because only the bare
    // final per-source filenames are probed here.
    for name in [
        SCREEN_FILE_NAME,
        source_file_name(SourceKind::SystemAudio),
        source_file_name(SourceKind::Mic),
        AUDIO_FILE_NAME,
    ] {
        let path = dir.join(name);
        if path.is_file() && path.metadata()?.len() > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

struct PresentSealAudioSource {
    pcm_path: PathBuf,
    fmt_path: PathBuf,
    bytes: Vec<u8>,
    format: AudioFormat,
}

enum SealAudioSource {
    Absent,
    Orphan,
    Present(PresentSealAudioSource),
}

impl SealAudioSource {
    fn combine_input(&self) -> Option<(&[u8], AudioFormat)> {
        match self {
            Self::Present(source) => Some((source.bytes.as_slice(), source.format)),
            Self::Absent | Self::Orphan => None,
        }
    }
}

fn read_seal_audio_source(dir: &Path, source: SourceKind) -> io::Result<SealAudioSource> {
    let pcm_path = dir.join(source_file_name(source));
    if !pcm_path.is_file() || pcm_path.metadata()?.len() == 0 {
        return Ok(SealAudioSource::Absent);
    }

    let fmt_path = dir.join(source_fmt_file_name(source));
    let fmt_bytes = match fs::read(&fmt_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(SealAudioSource::Orphan),
        Err(err) => return Err(err),
    };
    let Ok(format) = serde_json::from_slice::<AudioFormat>(&fmt_bytes) else {
        return Ok(SealAudioSource::Orphan);
    };

    Ok(SealAudioSource::Present(PresentSealAudioSource {
        bytes: fs::read(&pcm_path)?,
        pcm_path,
        fmt_path,
        format,
    }))
}

fn seal_audio_flac(dir: &Path) -> io::Result<()> {
    let mic = read_seal_audio_source(dir, SourceKind::Mic)?;
    let sys = read_seal_audio_source(dir, SourceKind::SystemAudio)?;
    if matches!(mic, SealAudioSource::Orphan) || matches!(sys, SealAudioSource::Orphan) {
        return Ok(());
    }

    let flac = observer_audio::combine_to_flac(mic.combine_input(), sys.combine_input())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    let Some(bytes) = flac else {
        return Ok(());
    };

    let path = dir.join(AUDIO_FILE_NAME);
    let mut file = File::create(&path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;

    for source in [&mic, &sys] {
        if let SealAudioSource::Present(source) = source {
            fs::remove_file(&source.pcm_path)?;
            match fs::remove_file(&source.fmt_path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
    }

    Ok(())
}

/// Compute and durably persist the honest LEN sidecar for a sealed segment.
/// Reads the sealed audio.flac duration if present; else uses the video end
/// hint (normal seal) or the ceiling (recovery, hint None). Written BEFORE the
/// atomic rename so a persist failure fails the seal visibly (dir stays
/// .incomplete for recovery) rather than falling through to a nominal LEN.
fn persist_seal_len(dir: &Path, video_end_secs: Option<f64>, period_secs: u64) -> io::Result<u64> {
    let audio_secs = match fs::read(dir.join(AUDIO_FILE_NAME)) {
        Ok(bytes) => observer_audio::flac_duration_secs(&bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    let len = observer_segment::duration::resolve_len_secs(audio_secs, video_end_secs, period_secs);
    let mut file = File::create(dir.join(LEN_FILE_NAME))?;
    file.write_all(len.to_string().as_bytes())?;
    file.sync_all()?;
    Ok(len)
}

fn drop_entry(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn seal_or_merge(incomplete: &Path, sealed: &Path) -> io::Result<()> {
    if !sealed.exists() {
        return fs::rename(incomplete, sealed);
    }

    for entry in fs::read_dir(incomplete)? {
        let entry = entry?;
        let source = entry.path();
        let target = sealed.join(entry.file_name());
        if target.exists() {
            drop_entry(&source)?;
        } else {
            fs::rename(&source, target)?;
        }
    }
    fs::remove_dir(incomplete)
}

fn remove_partial_media(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with(".partial") {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn absolute_string(path: &Path) -> io::Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.to_string_lossy().into_owned())
}

/// Outcome of the single-instance acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceLock {
    /// This process owns the per-session lock; proceed.
    Acquired,
    /// Another instance already owns it in this interactive session.
    AlreadyRunning,
}

/// Acquire the per-session single-instance lock.
#[cfg(not(windows))]
pub fn acquire_single_instance(_name: &str) -> InstanceLock {
    InstanceLock::Acquired
}

/// Acquire the per-session single-instance lock.
#[cfg(windows)]
pub fn acquire_single_instance(name: &str) -> InstanceLock {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let full_name = format!("Local\\{name}");
    let wide: Vec<u16> = full_name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) };
    let Ok(handle) = handle else {
        return InstanceLock::AlreadyRunning;
    };

    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let _held_for_process_lifetime = Box::leak(Box::new(handle));
    if already_exists {
        InstanceLock::AlreadyRunning
    } else {
        InstanceLock::Acquired
    }
}

/// A session/power lifecycle notification the pump can deliver to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemNotification {
    SessionLocked,
    SessionUnlocked,
    DisplayChanged,
    Suspending,
    Resumed,
    /// The owner pressed the configured global pause/resume hotkey. The shell maps
    /// this to a pause/resume toggle.
    HotkeyPressed,
}

/// The hotkey id we register; we only ever own one global hotkey at a time.
#[cfg_attr(not(windows), allow(dead_code))]
const HOTKEY_ID: i32 = 1;

#[cfg(not(windows))]
/// The session/power notification pump.
#[derive(Debug, Default)]
pub struct NotificationPump;

#[cfg(not(windows))]
impl NotificationPump {
    pub fn new() -> Self {
        Self
    }

    /// Construct a pump bound to the owner's global hotkey. No-op off Windows.
    pub fn with_hotkey(
        _desired: std::sync::Arc<std::sync::Mutex<observer_hotkey::HotkeyConfig>>,
        _outcome: std::sync::Arc<std::sync::Mutex<observer_hotkey::HotkeyRegistration>>,
    ) -> Self {
        Self
    }

    /// Drain any pending notifications. Empty on non-Windows hosts.
    pub fn poll(&mut self) -> Vec<SystemNotification> {
        Vec::new()
    }
}

#[cfg(windows)]
mod notification_pump {
    use std::sync::{Arc, Mutex, OnceLock};

    use observer_hotkey::{HotkeyConfig, HotkeyRegistration};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        ERROR_HOTKEY_ALREADY_REGISTERED, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
        MOD_SHIFT, MOD_WIN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW,
        TranslateMessage, HWND_MESSAGE, MSG, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PM_REMOVE,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_DISPLAYCHANGE, WM_HOTKEY, WM_POWERBROADCAST,
        WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    use super::{SystemNotification, HOTKEY_ID};

    static QUEUE: OnceLock<Mutex<Vec<SystemNotification>>> = OnceLock::new();

    fn queue() -> &'static Mutex<Vec<SystemNotification>> {
        QUEUE.get_or_init(|| Mutex::new(Vec::new()))
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let notification = match msg {
            WM_WTSSESSION_CHANGE if wparam.0 == WTS_SESSION_LOCK as usize => {
                Some(SystemNotification::SessionLocked)
            }
            WM_WTSSESSION_CHANGE if wparam.0 == WTS_SESSION_UNLOCK as usize => {
                Some(SystemNotification::SessionUnlocked)
            }
            WM_DISPLAYCHANGE => Some(SystemNotification::DisplayChanged),
            WM_POWERBROADCAST if wparam.0 == PBT_APMSUSPEND as usize => {
                Some(SystemNotification::Suspending)
            }
            WM_POWERBROADCAST if wparam.0 == PBT_APMRESUMESUSPEND as usize => {
                Some(SystemNotification::Resumed)
            }
            // Our global hotkey fired. We register it against this window, so the
            // message lands here (and the filtered PeekMessage below retrieves it).
            WM_HOTKEY if wparam.0 == HOTKEY_ID as usize => Some(SystemNotification::HotkeyPressed),
            _ => None,
        };
        if let Some(notification) = notification {
            queue().lock().unwrap().push(notification);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn wide_z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Win32 modifier mask for a config (always with `MOD_NOREPEAT` so a held
    /// combo fires once, not repeatedly).
    fn modifiers_for(config: &HotkeyConfig) -> HOT_KEY_MODIFIERS {
        let mut mods = MOD_NOREPEAT;
        if config.ctrl {
            mods |= MOD_CONTROL;
        }
        if config.alt {
            mods |= MOD_ALT;
        }
        if config.shift {
            mods |= MOD_SHIFT;
        }
        if config.win {
            mods |= MOD_WIN;
        }
        mods
    }

    /// Attempt to register `config` as the global hotkey on `hwnd`. Maps the
    /// single-registrant failure (`ERROR_HOTKEY_ALREADY_REGISTERED`) to the honest
    /// [`HotkeyRegistration::ComboTaken`] rather than a silent no-op.
    fn register_combo(hwnd: HWND, config: &HotkeyConfig) -> HotkeyRegistration {
        match unsafe { RegisterHotKey(hwnd, HOTKEY_ID, modifiers_for(config), config.vk) } {
            Ok(()) => HotkeyRegistration::Registered,
            Err(error) => {
                if error.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
                    HotkeyRegistration::ComboTaken
                } else {
                    eprintln!("hotkey: RegisterHotKey failed: {error}");
                    HotkeyRegistration::Failed
                }
            }
        }
    }

    /// Shared state for the optional global hotkey the pump manages: the owner's
    /// desired config (written by the shell over IPC) and the honest registration
    /// outcome the pump reports back (read by Settings).
    struct HotkeyBinding {
        desired: Arc<Mutex<HotkeyConfig>>,
        outcome: Arc<Mutex<HotkeyRegistration>>,
        /// The config we last attempted to register, so we only re-register on a
        /// real change instead of every poll.
        applied: Option<HotkeyConfig>,
        /// Whether a hotkey id is currently registered with the OS.
        registered: bool,
    }

    /// The session/power notification pump (and, optionally, the global hotkey).
    #[derive(Debug)]
    pub struct NotificationPump {
        hwnd: HWND,
        hotkey: Option<HotkeyBinding>,
    }

    impl std::fmt::Debug for HotkeyBinding {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("HotkeyBinding")
                .field("applied", &self.applied)
                .field("registered", &self.registered)
                .finish()
        }
    }

    impl NotificationPump {
        pub fn new() -> Self {
            Self {
                hwnd: Self::create_window(),
                hotkey: None,
            }
        }

        /// Construct a pump that also owns the owner's global pause/resume hotkey.
        /// The pump reconciles registration from `desired` on every [`poll`] (on
        /// its own thread, where `RegisterHotKey`/`WM_HOTKEY` must live) and writes
        /// the honest result into `outcome`.
        pub fn with_hotkey(
            desired: Arc<Mutex<HotkeyConfig>>,
            outcome: Arc<Mutex<HotkeyRegistration>>,
        ) -> Self {
            Self {
                hwnd: Self::create_window(),
                hotkey: Some(HotkeyBinding {
                    desired,
                    outcome,
                    applied: None,
                    registered: false,
                }),
            }
        }

        fn create_window() -> HWND {
            let class = wide_z("SolstoneNotificationPump");
            unsafe {
                let module = GetModuleHandleW(PCWSTR::null()).ok();
                let instance = module.map_or(HINSTANCE::default(), HINSTANCE::from);
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance,
                    lpszClassName: PCWSTR(class.as_ptr()),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    PCWSTR(class.as_ptr()),
                    PCWSTR(class.as_ptr()),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    None,
                    instance,
                    None,
                )
                .unwrap_or_default();
                if !hwnd.is_invalid() {
                    let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
                }
                hwnd
            }
        }

        /// Reconcile the OS hotkey registration with the owner's desired config.
        /// Only acts when the desired config changed since the last poll; writes
        /// the honest outcome back so Settings can show it.
        fn reconcile_hotkey(&mut self) {
            let Some(binding) = self.hotkey.as_mut() else {
                return;
            };
            let desired = binding
                .desired
                .lock()
                .map(|c| *c)
                .unwrap_or_else(|_| HotkeyConfig::default());
            if binding.applied == Some(desired) {
                return;
            }
            // Drop any currently-registered combo before (re)registering.
            if binding.registered {
                unsafe {
                    let _ = UnregisterHotKey(self.hwnd, HOTKEY_ID);
                }
                binding.registered = false;
            }
            let outcome = if desired.is_armed() {
                let result = register_combo(self.hwnd, &desired);
                binding.registered = result == HotkeyRegistration::Registered;
                result
            } else {
                HotkeyRegistration::Inactive
            };
            binding.applied = Some(desired);
            if let Ok(mut out) = binding.outcome.lock() {
                *out = outcome;
            }
        }

        /// Drain any pending notifications.
        pub fn poll(&mut self) -> Vec<SystemNotification> {
            self.reconcile_hotkey();
            unsafe {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, self.hwnd, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            let mut guard = queue().lock().unwrap();
            std::mem::take(&mut *guard)
        }
    }

    impl Drop for NotificationPump {
        fn drop(&mut self) {
            if let Some(binding) = &self.hotkey {
                if binding.registered {
                    unsafe {
                        let _ = UnregisterHotKey(self.hwnd, HOTKEY_ID);
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
pub use notification_pump::NotificationPump;

/// Real `%LocalAppData%`-backed segment filesystem.
#[derive(Debug)]
pub struct LocalSegmentFs {
    root: PathBuf,
    handles: BTreeMap<(SegmentKey, SourceKind), File>,
}

impl LocalSegmentFs {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            handles: BTreeMap::new(),
        }
    }
}

impl Default for LocalSegmentFs {
    fn default() -> Self {
        Self::new(segments_dir())
    }
}

impl SegmentFs for LocalSegmentFs {
    type Error = io::Error;

    fn open_incomplete(&mut self, key: SegmentKey) -> Result<String, Self::Error> {
        fs::create_dir_all(&self.root)?;
        let dir = incomplete_dir(&self.root, key);
        fs::create_dir_all(&dir)?;
        absolute_string(&dir)
    }

    fn write_chunk(&mut self, key: SegmentKey, chunk: &CaptureChunk) -> Result<(), Self::Error> {
        if chunk.source == SourceKind::Screen {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "screen frames are encoded through ScreenEncoder",
            ));
        }
        let dir = incomplete_dir(&self.root, key);
        fs::create_dir_all(&dir)?;
        let handle_key = (key, chunk.source);
        if let std::collections::btree_map::Entry::Vacant(entry) = self.handles.entry(handle_key) {
            if let Some(format) = chunk.format {
                let sidecar_path = dir.join(source_fmt_file_name(chunk.source));
                if !sidecar_path.exists() {
                    let mut sidecar = File::create(sidecar_path)?;
                    serde_json::to_writer(&mut sidecar, &format)
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                    sidecar.sync_all()?;
                }
            }
            let path = dir.join(source_file_name(chunk.source));
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            entry.insert(file);
        }
        let file = self.handles.get_mut(&handle_key).expect("handle inserted");
        file.write_all(&chunk.data)?;
        // Durably commit each chunk. Without this, a 5-minute segment's frames sit
        // in the OS write cache until finalize: a crash would lose the whole
        // in-flight segment, and recovery's usable-data check (file length) would
        // read 0 for a segment that actually captured data and wrongly quarantine
        // it. At the capped ~1 fps this is ~1 fsync/sec — negligible.
        file.sync_all()?;
        Ok(())
    }

    fn finalize(
        &mut self,
        key: SegmentKey,
        video_end_secs: Option<f64>,
    ) -> Result<(), Self::Error> {
        let to_drop: Vec<_> = self
            .handles
            .keys()
            .copied()
            .filter(|(handle_key, _)| *handle_key == key)
            .collect();
        for handle_key in to_drop {
            if let Some(mut file) = self.handles.remove(&handle_key) {
                file.flush()?;
                file.sync_all()?;
            }
        }
        let dir = incomplete_dir(&self.root, key);
        seal_audio_flac(&dir)?;
        persist_seal_len(&dir, video_end_secs, DEFAULT_SEGMENT_SECS)?;
        seal_or_merge(&dir, &sealed_dir(&self.root, key))
    }
}

/// Real `%LocalAppData%`-backed recovery filesystem (`observer-recovery` seam).
#[derive(Debug)]
pub struct LocalRecoveryFs {
    root: PathBuf,
}

impl LocalRecoveryFs {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Default for LocalRecoveryFs {
    fn default() -> Self {
        Self::new(segments_dir())
    }
}

impl RecoveryFs for LocalRecoveryFs {
    type Error = io::Error;

    fn scan_incomplete(&mut self) -> Result<Vec<StaleSegment>, Self::Error> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }

        // Boundary-based staleness. The single-instance named mutex is acquired
        // at boot, before the engine and its capture sources start (a second
        // launch exits immediately), so recovery is guaranteed no concurrent
        // writer — there is no live writer to race. That removes the need for
        // the old age/mtime heuristic, which could delay sealing a genuinely
        // orphaned segment by up to one segment-plus-margin window. The only
        // directory recovery must leave untouched is the one whose aligned
        // window contains *now*: the engine re-opens and *continues* it on
        // restart. Every other `.incomplete` (a past window the prior run was
        // sealing when it crashed — including one it had only just crossed into
        // — or an anomalous future window from a backward clock step) is a real
        // orphan and is swept on the spot.
        let now_epoch_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let mut stale = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(index_text) = name.strip_suffix(".incomplete") else {
                continue;
            };
            let Ok(index) = index_text.parse::<u64>() else {
                continue;
            };
            let key = SegmentKey {
                boundary_epoch_secs: index * DEFAULT_SEGMENT_SECS,
                index,
            };
            if is_live_segment(key, now_epoch_secs, DEFAULT_SEGMENT_SECS) {
                continue;
            }
            stale.push(StaleSegment {
                key,
                path: absolute_string(&path)?,
                has_usable_data: has_sealable_media(&path)?,
            });
        }
        stale.sort_by_key(|seg| seg.key);
        Ok(stale)
    }

    fn finalize(&mut self, seg: &StaleSegment) -> Result<(), Self::Error> {
        // Never abandon usable captured data: a truncated PCM is still usable,
        // a moov-less mp4 is not. Drop dead screen partials before sealing the
        // remaining final media files.
        let dir = Path::new(&seg.path);
        remove_partial_media(dir)?;
        seal_audio_flac(dir)?;
        persist_seal_len(dir, None, DEFAULT_SEGMENT_SECS)?;
        seal_or_merge(dir, &sealed_dir(&self.root, seg.key))
    }

    fn quarantine(&mut self, seg: &StaleSegment) -> Result<(), Self::Error> {
        let quarantine = self.root.join("quarantine");
        fs::create_dir_all(&quarantine)?;
        let target = quarantine.join(format!("{}.incomplete", seg.key.index));
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        fs::rename(&seg.path, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_segment::segment_for;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "solstone-platform-win-{name}-{}-{id}",
            std::process::id()
        ))
    }

    fn key(index: u64) -> SegmentKey {
        SegmentKey {
            boundary_epoch_secs: index * DEFAULT_SEGMENT_SECS,
            index,
        }
    }

    fn chunk(source: SourceKind, data: &[u8]) -> CaptureChunk {
        CaptureChunk {
            source,
            seq: 0,
            data: data.to_vec(),
            format: None,
        }
    }

    fn audio_format() -> AudioFormat {
        AudioFormat {
            sample_rate_hz: 16_000,
            channels: 1,
            bits_per_sample: 16,
            is_float: false,
        }
    }

    fn pcm_i16(samples: &[i16]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    fn audio_chunk(source: SourceKind, samples: &[i16]) -> CaptureChunk {
        CaptureChunk {
            source,
            seq: 0,
            data: pcm_i16(samples),
            format: Some(audio_format()),
        }
    }

    fn write_audio_file(dir: &Path, source: SourceKind, samples: &[i16]) {
        fs::write(dir.join(source_file_name(source)), pcm_i16(samples)).unwrap();
        fs::write(
            dir.join(source_fmt_file_name(source)),
            serde_json::to_vec(&audio_format()).unwrap(),
        )
        .unwrap();
    }

    fn len_sidecar(root: &Path, index: u64) -> String {
        fs::read_to_string(root.join(format!("{index}/{LEN_FILE_NAME}"))).unwrap()
    }

    #[test]
    fn data_root_is_under_solstone() {
        assert!(local_data_root().ends_with("Solstone") || local_data_root().ends_with("solstone"));
    }

    #[test]
    fn single_instance_acquires_on_host() {
        assert_eq!(acquire_single_instance("test"), InstanceLock::Acquired);
    }

    #[test]
    fn segment_lifecycle_writes_audio_source_files_and_finalizes() {
        let root = temp_root("segment");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(7);

        let path = fs_impl.open_incomplete(key).unwrap();
        assert!(PathBuf::from(path).ends_with("7.incomplete"));
        assert!(root.join("7.incomplete").is_dir());

        fs_impl
            .write_chunk(key, &audio_chunk(SourceKind::SystemAudio, &[1000; 16]))
            .unwrap();
        fs_impl
            .write_chunk(key, &audio_chunk(SourceKind::Mic, &[2000; 16]))
            .unwrap();
        fs_impl.finalize(key, None).unwrap();

        assert!(!root.join("7.incomplete").exists());
        assert!(root.join(format!("7/{AUDIO_FILE_NAME}")).is_file());
        assert!(!root.join("7/system-audio.pcm").exists());
        assert!(!root.join("7/mic.pcm").exists());
        assert!(!root.join("7/system-audio.fmt.json").exists());
        assert!(!root.join("7/mic.fmt.json").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn normal_seal_persists_len_from_audio_flac_duration() {
        let root = temp_root("normal-audio-len");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(15);
        let samples = vec![1000; 48_000];

        fs_impl.open_incomplete(key).unwrap();
        fs_impl
            .write_chunk(key, &audio_chunk(SourceKind::Mic, &samples))
            .unwrap();
        fs_impl.finalize(key, Some(120.0)).unwrap();

        assert_eq!(len_sidecar(&root, 15), "3");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn normal_seal_video_only_persists_len_from_video_end_hint() {
        let root = temp_root("normal-video-len");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(16);

        fs_impl.open_incomplete(key).unwrap();
        fs::write(root.join("16.incomplete").join(SCREEN_FILE_NAME), b"mp4").unwrap();
        fs_impl.finalize(key, Some(120.0)).unwrap();

        assert_eq!(len_sidecar(&root, 16), "120");

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn normal_seal_len_persist_failure_leaves_incomplete_unsealed() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("normal-len-failure");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(19);

        fs_impl.open_incomplete(key).unwrap();
        let incomplete = root.join("19.incomplete");
        fs::write(incomplete.join(SCREEN_FILE_NAME), b"mp4").unwrap();

        let mut perms = fs::metadata(&incomplete).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&incomplete, perms).unwrap();

        let err = fs_impl.finalize(key, Some(120.0)).unwrap_err();

        let mut perms = fs::metadata(&incomplete).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&incomplete, perms).unwrap();

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(incomplete.is_dir());
        assert!(!root.join("19").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sidecar_is_written_once_across_reopened_segment_fs() {
        let root = temp_root("sidecar-once");
        let _ = fs::remove_dir_all(&root);
        let key = key(11);
        let first = AudioFormat {
            sample_rate_hz: 16_000,
            channels: 1,
            bits_per_sample: 16,
            is_float: false,
        };
        let second = AudioFormat {
            sample_rate_hz: 48_000,
            channels: 2,
            bits_per_sample: 32,
            is_float: true,
        };

        let mut fs_impl = LocalSegmentFs::new(root.clone());
        fs_impl.open_incomplete(key).unwrap();
        fs_impl
            .write_chunk(
                key,
                &CaptureChunk {
                    source: SourceKind::SystemAudio,
                    seq: 0,
                    data: pcm_i16(&[1000; 16]),
                    format: Some(first),
                },
            )
            .unwrap();
        drop(fs_impl);

        let mut fs_impl = LocalSegmentFs::new(root.clone());
        fs_impl
            .write_chunk(
                key,
                &CaptureChunk {
                    source: SourceKind::SystemAudio,
                    seq: 1,
                    data: pcm_i16(&[2000; 16]),
                    format: Some(second),
                },
            )
            .unwrap();

        let sidecar: AudioFormat = serde_json::from_slice(
            &fs::read(root.join("11.incomplete/system-audio.fmt.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar, first);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn segment_finalize_leaves_all_audio_raw_when_any_source_is_orphan() {
        let root = temp_root("mixed-orphan");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(14);

        fs_impl.open_incomplete(key).unwrap();
        fs_impl
            .write_chunk(key, &audio_chunk(SourceKind::Mic, &[2000; 16]))
            .unwrap();
        let incomplete = root.join("14.incomplete");
        fs::write(
            incomplete.join(source_file_name(SourceKind::SystemAudio)),
            pcm_i16(&[1000; 16]),
        )
        .unwrap();

        assert!(has_sealable_media(&incomplete).unwrap());

        fs_impl.finalize(key, None).unwrap();

        let sealed = root.join("14");
        assert!(!sealed.join(AUDIO_FILE_NAME).exists());
        assert!(sealed.join("mic.pcm").is_file());
        assert!(sealed.join("system-audio.pcm").is_file());
        assert!(sealed.join("mic.fmt.json").is_file());
        assert!(!sealed.join("system-audio.fmt.json").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn segment_fs_rejects_screen_chunks() {
        let root = temp_root("segment-screen-reject");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(8);

        fs_impl.open_incomplete(key).unwrap();
        let error = fs_impl
            .write_chunk(key, &chunk(SourceKind::Screen, b"raw-rgba"))
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!root
            .join(format!("8.incomplete/{SCREEN_FILE_NAME}"))
            .exists());

        let _ = fs::remove_dir_all(&root);
    }

    fn now_epoch_secs() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn recovery_scan_keeps_current_boundary_sweeps_past() {
        let root = temp_root("staleness");
        let _ = fs::remove_dir_all(&root);

        // A past-window orphan (index 1 -> boundary epoch 300, i.e. 1970): the
        // prior run was sealing it when it crashed. It must be swept regardless
        // of its (here, fresh) mtime — boundary identity, not age, decides.
        fs::create_dir_all(root.join("1.incomplete")).unwrap();
        fs::write(
            root.join(format!("1.incomplete/{SCREEN_FILE_NAME}")),
            b"orphan",
        )
        .unwrap();

        // The live segment for *now*: its aligned window contains the wall
        // clock, so the engine will re-open and continue it. Recovery must
        // leave it untouched.
        let live = segment_for(now_epoch_secs(), DEFAULT_SEGMENT_SECS);
        fs::create_dir_all(root.join(format!("{}.incomplete", live.index))).unwrap();
        fs::write(
            root.join(format!("{}.incomplete/{SCREEN_FILE_NAME}", live.index)),
            b"live",
        )
        .unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();

        // Only the past-window orphan is returned; the live segment is skipped.
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].key, key(1));
        assert!(stale[0].has_usable_data);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_finalizes_just_crossed_prior_boundary_immediately() {
        // The cross-boundary stale-finalize case W1 left open: the prior run
        // crossed into the current window, then crashed seconds later, leaving
        // the immediately-preceding window's `.incomplete` with a *fresh* mtime.
        // The old age guard would have skipped it for up to ~360 s; boundary
        // recovery seals it on the spot and never loses its usable data.
        let root = temp_root("cross-boundary");
        let _ = fs::remove_dir_all(&root);

        let live = segment_for(now_epoch_secs(), DEFAULT_SEGMENT_SECS);
        let prior_index = live.index - 1; // the window immediately before now

        fs::create_dir_all(root.join(format!("{prior_index}.incomplete"))).unwrap();
        fs::write(
            root.join(format!("{prior_index}.incomplete/{SCREEN_FILE_NAME}")),
            b"just-crossed",
        )
        .unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].key.index, prior_index);
        assert!(
            stale[0].has_usable_data,
            "a just-crossed orphan with captured frames must be finalized, not lost"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_seals_partial_screen_with_usable_audio_and_drops_partial() {
        let root = temp_root("partial-audio");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("2.incomplete");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(
            orphan.join(format!("{SCREEN_FILE_NAME}.partial")),
            b"moovless",
        )
        .unwrap();
        write_audio_file(&orphan, SourceKind::SystemAudio, &[1000; 16]);

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].has_usable_data);

        recovery.finalize(&stale[0]).unwrap();

        assert!(root.join(format!("2/{AUDIO_FILE_NAME}")).is_file());
        assert!(!root.join("2/system-audio.pcm").exists());
        assert!(!root.join("2/system-audio.fmt.json").exists());
        assert!(!root.join(format!("2/{SCREEN_FILE_NAME}.partial")).exists());
        assert!(!root.join("2.incomplete").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_seal_persists_len_from_audio_flac_duration() {
        let root = temp_root("recovery-audio-len");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("17.incomplete");
        let samples = vec![1000; 48_000];
        fs::create_dir_all(&orphan).unwrap();
        write_audio_file(&orphan, SourceKind::Mic, &samples);

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);

        recovery.finalize(&stale[0]).unwrap();

        assert_eq!(len_sidecar(&root, 17), "3");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_seal_orphaned_audio_uses_ceiling_len() {
        let root = temp_root("recovery-orphan-len");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("18.incomplete");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("mic.pcm"), pcm_i16(&[1000; 16])).unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);

        recovery.finalize(&stale[0]).unwrap();

        assert_eq!(len_sidecar(&root, 18), "300");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn local_segment_finalize_merges_when_sealed_dir_already_exists() {
        let root = temp_root("normal-merge-existing");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(20);
        let sealed = root.join("20");

        fs_impl.open_incomplete(key).unwrap();
        fs_impl
            .write_chunk(key, &audio_chunk(SourceKind::Mic, &[2000; 16]))
            .unwrap();
        fs::create_dir_all(&sealed).unwrap();
        fs::write(sealed.join(SCREEN_FILE_NAME), b"existing-mp4").unwrap();
        fs::write(sealed.join(LEN_FILE_NAME), b"99").unwrap();

        fs_impl.finalize(key, None).unwrap();

        assert!(!root.join("20.incomplete").exists());
        assert_eq!(
            fs::read(sealed.join(SCREEN_FILE_NAME)).unwrap(),
            b"existing-mp4"
        );
        assert!(sealed.join(AUDIO_FILE_NAME).is_file());
        assert_eq!(
            fs::read_to_string(sealed.join(LEN_FILE_NAME)).unwrap(),
            "99"
        );

        let audio_files = fs::read_dir(&sealed)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() == AUDIO_FILE_NAME)
            .count();
        assert_eq!(audio_files, 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_finalize_merges_with_existing_sealed_target() {
        let root = temp_root("recovery-merge-existing");
        let _ = fs::remove_dir_all(&root);
        let sealed = root.join("21");
        let orphan = root.join("21.incomplete");
        fs::create_dir_all(&sealed).unwrap();
        fs::create_dir_all(&orphan).unwrap();
        fs::write(sealed.join(SCREEN_FILE_NAME), b"existing-mp4").unwrap();
        fs::write(sealed.join(LEN_FILE_NAME), b"10").unwrap();
        fs::write(orphan.join(AUDIO_FILE_NAME), b"flac").unwrap();
        fs::write(orphan.join(LEN_FILE_NAME), b"3").unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let seg = StaleSegment {
            key: key(21),
            path: absolute_string(&orphan).unwrap(),
            has_usable_data: true,
        };

        recovery.finalize(&seg).unwrap();

        assert!(!orphan.exists());
        assert_eq!(
            fs::read(sealed.join(SCREEN_FILE_NAME)).unwrap(),
            b"existing-mp4"
        );
        assert_eq!(fs::read(sealed.join(AUDIO_FILE_NAME)).unwrap(), b"flac");
        assert_eq!(
            fs::read_to_string(sealed.join(LEN_FILE_NAME)).unwrap(),
            "10"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_converges_after_audio_flac_written_before_pcm_delete() {
        let root = temp_root("seal-crash-before-delete");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("6.incomplete");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join(AUDIO_FILE_NAME), b"stale-flac").unwrap();
        write_audio_file(&orphan, SourceKind::Mic, &[3000; 16]);

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].has_usable_data);

        recovery.finalize(&stale[0]).unwrap();

        assert!(root.join(format!("6/{AUDIO_FILE_NAME}")).is_file());
        assert!(!root.join("6/mic.pcm").exists());
        assert!(!root.join("6/mic.fmt.json").exists());
        assert!(!root.join("6.incomplete").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_seals_audio_flac_only_after_pcm_delete_before_rename() {
        let root = temp_root("seal-crash-after-delete");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("7.incomplete");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join(AUDIO_FILE_NAME), b"flac").unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].has_usable_data);

        recovery.finalize(&stale[0]).unwrap();

        assert_eq!(
            fs::read(root.join(format!("7/{AUDIO_FILE_NAME}"))).unwrap(),
            b"flac"
        );
        assert!(!root.join("7.incomplete").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finalize_with_no_audio_sources_writes_no_audio_flac() {
        let root = temp_root("no-audio");
        let _ = fs::remove_dir_all(&root);
        let mut fs_impl = LocalSegmentFs::new(root.clone());
        let key = key(12);

        fs_impl.open_incomplete(key).unwrap();
        fs_impl.finalize(key, None).unwrap();

        assert!(root.join("12").is_dir());
        assert!(!root.join(format!("12/{AUDIO_FILE_NAME}")).exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_leaves_pre_upgrade_pcm_orphan_without_sidecar() {
        let root = temp_root("pcm-orphan");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("13.incomplete");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("system-audio.pcm"), pcm_i16(&[1000; 16])).unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].has_usable_data);

        recovery.finalize(&stale[0]).unwrap();

        assert!(root.join("13/system-audio.pcm").is_file());
        assert!(!root.join(format!("13/{AUDIO_FILE_NAME}")).exists());
        assert!(!root.join("13.incomplete").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_quarantines_partial_only_orphan() {
        let root = temp_root("partial-only");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("3.incomplete");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(
            orphan.join(format!("{SCREEN_FILE_NAME}.partial")),
            b"moovless",
        )
        .unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(!stale[0].has_usable_data);

        recovery.quarantine(&stale[0]).unwrap();

        assert!(root.join("quarantine/3.incomplete").is_dir());
        assert!(root
            .join(format!(
                "quarantine/3.incomplete/{SCREEN_FILE_NAME}.partial"
            ))
            .is_file());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_seals_finalized_screen_mp4_without_partial() {
        let root = temp_root("final-screen");
        let _ = fs::remove_dir_all(&root);
        let orphan = root.join("4.incomplete");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join(SCREEN_FILE_NAME), b"mp4").unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].has_usable_data);

        recovery.finalize(&stale[0]).unwrap();

        assert!(root.join(format!("4/{SCREEN_FILE_NAME}")).is_file());
        assert!(!root.join("4.incomplete").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_quarantines_empty_orphan() {
        let root = temp_root("empty-orphan");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("5.incomplete")).unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(!stale[0].has_usable_data);

        recovery.quarantine(&stale[0]).unwrap();

        assert!(root.join("quarantine/5.incomplete").is_dir());
        assert!(!root.join("5.incomplete").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_finalize_and_quarantine_rename_dirs() {
        let root = temp_root("recovery");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("3.incomplete")).unwrap();
        fs::write(root.join("3.incomplete/system-audio.pcm"), b"pcm").unwrap();
        fs::create_dir_all(root.join("4.incomplete")).unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let seg3 = StaleSegment {
            key: key(3),
            path: absolute_string(&root.join("3.incomplete")).unwrap(),
            has_usable_data: true,
        };
        let seg4 = StaleSegment {
            key: key(4),
            path: absolute_string(&root.join("4.incomplete")).unwrap(),
            has_usable_data: false,
        };

        recovery.finalize(&seg3).unwrap();
        recovery.quarantine(&seg4).unwrap();

        assert!(root.join("3").is_dir());
        assert!(root.join("quarantine/4.incomplete").is_dir());
        assert!(!root.join("3.incomplete").exists());
        assert!(!root.join("4.incomplete").exists());

        let _ = fs::remove_dir_all(&root);
    }
}
