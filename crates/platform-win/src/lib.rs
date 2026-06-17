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

use observer_model::{CaptureChunk, SegmentKey, SourceKind, SCREEN_FILE_NAME};
use observer_recovery::{RecoveryFs, StaleSegment};
use observer_segment::{is_live_segment, SegmentFs, DEFAULT_SEGMENT_SECS};

pub mod autostart;

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

fn has_sealable_media(dir: &Path) -> io::Result<bool> {
    // Partial files are never counted as sealable media because only the bare
    // final per-source filenames are probed here.
    for source in [SourceKind::Screen, SourceKind::SystemAudio, SourceKind::Mic] {
        let path = dir.join(source_file_name(source));
        if path.is_file() && path.metadata()?.len() > 0 {
            return Ok(true);
        }
    }
    Ok(false)
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
}

#[cfg(not(windows))]
/// The session/power notification pump.
#[derive(Debug, Default)]
pub struct NotificationPump;

#[cfg(not(windows))]
impl NotificationPump {
    pub fn new() -> Self {
        Self
    }

    /// Drain any pending notifications. Empty on non-Windows hosts.
    pub fn poll(&mut self) -> Vec<SystemNotification> {
        Vec::new()
    }
}

#[cfg(windows)]
mod notification_pump {
    use std::sync::{Mutex, OnceLock};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, RegisterClassW,
        TranslateMessage, HWND_MESSAGE, MSG, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PM_REMOVE,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_DISPLAYCHANGE, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE,
        WNDCLASSW, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    use super::SystemNotification;

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

    /// The session/power notification pump.
    #[derive(Debug)]
    pub struct NotificationPump {
        hwnd: HWND,
    }

    impl NotificationPump {
        pub fn new() -> Self {
            let class = wide_z("SolstoneNotificationPump");
            let hwnd = unsafe {
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
            };
            Self { hwnd }
        }

        /// Drain any pending notifications.
        pub fn poll(&mut self) -> Vec<SystemNotification> {
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
        if !self.handles.contains_key(&handle_key) {
            let path = dir.join(source_file_name(chunk.source));
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            self.handles.insert(handle_key, file);
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

    fn finalize(&mut self, key: SegmentKey) -> Result<(), Self::Error> {
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
        fs::rename(incomplete_dir(&self.root, key), sealed_dir(&self.root, key))
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
        remove_partial_media(Path::new(&seg.path))?;
        fs::rename(&seg.path, sealed_dir(&self.root, seg.key))
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
        }
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
            .write_chunk(key, &chunk(SourceKind::SystemAudio, b"pcm-sys"))
            .unwrap();
        fs_impl
            .write_chunk(key, &chunk(SourceKind::Mic, b"pcm-mic"))
            .unwrap();
        fs_impl.finalize(key).unwrap();

        assert!(!root.join("7.incomplete").exists());
        assert_eq!(
            fs::read(root.join("7/system-audio.pcm")).unwrap(),
            b"pcm-sys"
        );
        assert_eq!(fs::read(root.join("7/mic.pcm")).unwrap(), b"pcm-mic");

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
        fs::write(orphan.join("system-audio.pcm"), b"pcm").unwrap();

        let mut recovery = LocalRecoveryFs::new(root.clone());
        let stale = recovery.scan_incomplete().unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].has_usable_data);

        recovery.finalize(&stale[0]).unwrap();

        assert!(root.join("2/system-audio.pcm").is_file());
        assert!(!root.join(format!("2/{SCREEN_FILE_NAME}.partial")).exists());
        assert!(!root.join("2.incomplete").exists());

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
