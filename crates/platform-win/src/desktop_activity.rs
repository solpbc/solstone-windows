// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop activity source probe, Win32 WinEvent/session/idle integration, and URL helper containment.

use std::path::Path;
#[cfg(not(windows))]
use std::process::{Child, Command, Stdio};
#[cfg(all(windows, test))]
use std::process::{Command, Stdio};
#[cfg(windows)]
mod helper_process;
#[cfg(windows)]
use helper_process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use observer_model::desktop_activity::{
    ActivityState, DesktopActivityProbeEvent, DesktopActivitySource, DesktopActivityUrlRequest,
    DesktopActivityUrlResult,
};
#[cfg(windows)]
use observer_model::ErrorReason;
use observer_model::{SourceError, SourceState};

/// Query Windows session eligibility using WTSQuerySessionInformationW.
/// Returns true only when connect state is WTSActive and session state is unlocked.
#[cfg(windows)]
pub fn is_session_eligible() -> bool {
    use windows::core::PWSTR;
    use windows::Win32::System::RemoteDesktop::{
        WTSActive, WTSFreeMemory, WTSQuerySessionInformationW, WTSSessionInfoEx,
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_UNLOCK,
    };

    unsafe {
        let mut buffer = PWSTR::null();
        let mut bytes_returned = 0u32;
        if WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes_returned,
        )
        .is_err()
            || buffer.is_null()
        {
            return false;
        }

        let info = buffer.as_ptr() as *const windows::Win32::System::RemoteDesktop::WTSINFOEXW;
        let (conn_state, session_flags) = if (bytes_returned as usize)
            >= std::mem::size_of::<windows::Win32::System::RemoteDesktop::WTSINFOEXW>()
        {
            if (*info).Level != 1 {
                WTSFreeMemory(buffer.as_ptr() as *mut _);
                return false;
            }
            let level1 = &(*info).Data.WTSInfoExLevel1;
            (level1.SessionState, level1.SessionFlags)
        } else {
            WTSFreeMemory(buffer.as_ptr() as *mut _);
            return false;
        };

        WTSFreeMemory(buffer.as_ptr() as *mut _);

        conn_state == WTSActive && (session_flags as u32) == WTS_SESSIONSTATE_UNLOCK
    }
}

#[cfg(not(windows))]
pub fn is_session_eligible() -> bool {
    false
}

static PUMP_DEGRADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_pump_degraded(degraded: bool) {
    PUMP_DEGRADED.store(degraded, std::sync::atomic::Ordering::SeqCst);
}

pub fn is_pump_degraded() -> bool {
    PUMP_DEGRADED.load(std::sync::atomic::Ordering::SeqCst)
}

static PUMP_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn is_pump_ready() -> bool {
    PUMP_READY.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn mark_pump_registration_finished(ready: bool) {
    PUMP_READY.store(ready, std::sync::atomic::Ordering::SeqCst);
}

/// Query FileDescription from an executable's version resource.
#[cfg(windows)]
pub fn get_file_description(exe_path: &str) -> Option<String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    let wide: Vec<u16> = OsStr::new(exe_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }

        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(PCWSTR(wide.as_ptr()), 0, size, data.as_mut_ptr() as *mut _).is_err()
        {
            return None;
        }

        let subblocks = [
            r"\StringFileInfo\040904b0\FileDescription",
            r"\StringFileInfo\040904e4\FileDescription",
            r"\StringFileInfo\000004b0\FileDescription",
        ];

        for subblock in subblocks {
            let wide_sub: Vec<u16> = OsStr::new(subblock)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            if VerQueryValueW(
                data.as_ptr() as *const _,
                PCWSTR(wide_sub.as_ptr()),
                &mut ptr,
                &mut len,
            )
            .as_bool()
                && !ptr.is_null()
                && len > 1
            {
                let slice = std::slice::from_raw_parts(ptr as *const u16, (len - 1) as usize);
                let desc = String::from_utf16_lossy(slice);
                let trimmed = desc.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        None
    }
}

#[cfg(not(windows))]
pub fn get_file_description(_exe_path: &str) -> Option<String> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiaFactUnavailable;

/// Abstract UI Automation node interface for bounded tree walking.
pub trait UiaNode: Sized {
    fn control_type(&self) -> Result<i32, UiaFactUnavailable>;
    fn is_password(&self) -> Result<bool, UiaFactUnavailable>;
    fn is_control_element(&self) -> Result<bool, UiaFactUnavailable>;
    fn value_property(&self) -> Option<String>;
    fn legacy_value_property(&self) -> Option<String>;
    fn first_child(&self) -> Result<Option<Self>, UiaFactUnavailable>;
    fn next_sibling(&self) -> Result<Option<Self>, UiaFactUnavailable>;
}

enum WalkResult {
    Found(String),
    NotFound,
    DocumentReady,
    Stop,
}

fn walk_node<N: UiaNode>(
    node: &N,
    depth: usize,
    count: &mut usize,
    start: Instant,
    deadline: Duration,
    read_values: bool,
) -> WalkResult {
    *count += 1;
    if *count > 400 || start.elapsed() >= deadline {
        return WalkResult::Stop;
    }

    const UIA_DOCUMENT_CONTROL_TYPE_ID: i32 = 50030;

    match node.is_password() {
        Err(_) => return WalkResult::Stop,
        Ok(true) => match node.control_type() {
            Ok(ct) if ct == UIA_DOCUMENT_CONTROL_TYPE_ID => return WalkResult::Stop,
            Err(_) => return WalkResult::Stop,
            Ok(_) => return WalkResult::NotFound,
        },
        Ok(false) => {}
    }

    let control_type = match node.control_type() {
        Ok(ct) => ct,
        Err(_) => return WalkResult::Stop,
    };

    if control_type == UIA_DOCUMENT_CONTROL_TYPE_ID {
        match node.is_control_element() {
            Err(_) => return WalkResult::Stop,
            // Browser accessibility can initially expose a non-control Document
            // placeholder. It supplies no URL and is never a content subtree.
            Ok(false) => return WalkResult::NotFound,
            Ok(true) => {}
        }
        if !read_values {
            return WalkResult::DocumentReady;
        }
        if start.elapsed() >= deadline {
            return WalkResult::Stop;
        }
        let val_str = node.value_property();
        if start.elapsed() >= deadline {
            return WalkResult::Stop;
        }
        let leg_str = node.legacy_value_property();

        let norm_val = val_str.as_deref().and_then(observer_model::normalize_url);
        let norm_leg = leg_str.as_deref().and_then(observer_model::normalize_url);

        return match (norm_val, norm_leg) {
            (Some(v), Some(l)) => {
                if v == l {
                    WalkResult::Found(v)
                } else {
                    WalkResult::Stop
                }
            }
            (Some(v), None) => WalkResult::Found(v),
            (None, Some(l)) => WalkResult::Found(l),
            (None, None) => WalkResult::Stop,
        };
    }

    if depth >= 24 || *count >= 400 || start.elapsed() >= deadline {
        return WalkResult::Stop;
    }
    {
        let first_child = match node.first_child() {
            Ok(child) => child,
            Err(_) => return WalkResult::Stop,
        };
        if let Some(first_child) = first_child {
            let mut curr = Some(first_child);
            while let Some(child) = curr {
                match walk_node(&child, depth + 1, count, start, deadline, read_values) {
                    WalkResult::Found(url) => return WalkResult::Found(url),
                    WalkResult::DocumentReady => return WalkResult::DocumentReady,
                    WalkResult::Stop => return WalkResult::Stop,
                    WalkResult::NotFound => {}
                }
                if *count >= 400 || start.elapsed() >= deadline {
                    return WalkResult::Stop;
                }
                curr = match child.next_sibling() {
                    Ok(sibling) => sibling,
                    Err(_) => return WalkResult::Stop,
                };
            }
        }
    }

    WalkResult::NotFound
}

/// Pure bounded UI Automation tree walk.
pub fn walk_uia_tree<N: UiaNode>(root: &N) -> Option<String> {
    let start = Instant::now();
    let deadline = Duration::from_secs(2);
    let mut count = 0usize;
    match walk_node(root, 0, &mut count, start, deadline, true) {
        WalkResult::Found(url) => Some(url),
        WalkResult::NotFound | WalkResult::DocumentReady | WalkResult::Stop => None,
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Control-view discovery can initialize Chromium accessibility. It reads only
/// password/type metadata and navigation. Only the subsequent RAW pass can read
/// a Document URL, after checking every raw ancestor. Both passes share limits.
fn walk_uia_views<N: UiaNode>(
    control_root: &N,
    raw_root: impl FnOnce() -> Option<N>,
) -> Option<String> {
    let start = Instant::now();
    let deadline = Duration::from_secs(2);
    let mut count = 0;
    if matches!(
        walk_node(control_root, 0, &mut count, start, deadline, false),
        WalkResult::Stop
    ) {
        return None;
    }
    if count >= 400 || start.elapsed() >= deadline {
        return None;
    }
    let raw_root = raw_root()?;
    match walk_node(&raw_root, 0, &mut count, start, deadline, true) {
        WalkResult::Found(url) => Some(url),
        WalkResult::NotFound | WalkResult::DocumentReady | WalkResult::Stop => None,
    }
}

#[cfg(windows)]
pub struct ProductionUiaNode {
    pub element: windows::Win32::UI::Accessibility::IUIAutomationElement,
    pub walker: windows::Win32::UI::Accessibility::IUIAutomationTreeWalker,
}

#[cfg(windows)]
impl UiaNode for ProductionUiaNode {
    fn control_type(&self) -> Result<i32, UiaFactUnavailable> {
        unsafe {
            self.element
                .CurrentControlType()
                .map(|ct| ct.0)
                .map_err(|_| UiaFactUnavailable)
        }
    }

    fn is_password(&self) -> Result<bool, UiaFactUnavailable> {
        unsafe {
            self.element
                .CurrentIsPassword()
                .map(|b| b.as_bool())
                .map_err(|_| UiaFactUnavailable)
        }
    }

    fn is_control_element(&self) -> Result<bool, UiaFactUnavailable> {
        unsafe {
            self.element
                .CurrentIsControlElement()
                .map(|b| b.as_bool())
                .map_err(|_| UiaFactUnavailable)
        }
    }

    fn value_property(&self) -> Option<String> {
        use windows::core::BSTR;
        use windows::Win32::UI::Accessibility::UIA_ValueValuePropertyId;
        unsafe {
            self.element
                .GetCurrentPropertyValue(UIA_ValueValuePropertyId)
                .ok()
                .and_then(|v| BSTR::try_from(&v).ok().map(|b| b.to_string()))
        }
    }

    fn legacy_value_property(&self) -> Option<String> {
        use windows::core::BSTR;
        use windows::Win32::UI::Accessibility::UIA_LegacyIAccessibleValuePropertyId;
        unsafe {
            self.element
                .GetCurrentPropertyValue(UIA_LegacyIAccessibleValuePropertyId)
                .ok()
                .and_then(|v| BSTR::try_from(&v).ok().map(|b| b.to_string()))
        }
    }

    fn first_child(&self) -> Result<Option<Self>, UiaFactUnavailable> {
        self.navigate(true)
    }

    fn next_sibling(&self) -> Result<Option<Self>, UiaFactUnavailable> {
        self.navigate(false)
    }
}

#[cfg(windows)]
impl ProductionUiaNode {
    fn navigate(&self, first_child: bool) -> Result<Option<Self>, UiaFactUnavailable> {
        use windows::core::Interface;
        use windows::Win32::UI::Accessibility::IUIAutomationElement;
        // The generated non-null return wrapper maps S_OK/null (normal end of
        // a branch) to an error. Preserve the HRESULT separately so a provider
        // failure cannot skip an unread subtree and admit a later Document.
        unsafe {
            let mut raw = std::ptr::null_mut();
            let vtable = Interface::vtable(&self.walker);
            let navigate = if first_child {
                vtable.GetFirstChildElement
            } else {
                vtable.GetNextSiblingElement
            };
            let status = navigate(self.walker.as_raw(), self.element.as_raw(), &mut raw);
            let element = if raw.is_null() {
                None
            } else {
                // A successful COM out-parameter transfers exactly one reference.
                Some(IUIAutomationElement::from_raw(raw))
            };
            status.ok().map_err(|_| UiaFactUnavailable)?;
            Ok(element.map(|element| Self {
                element,
                walker: self.walker.clone(),
            }))
        }
    }
}

#[cfg(windows)]
fn uia_root(hwnd: windows::Win32::Foundation::HWND, control: bool) -> Option<ProductionUiaNode> {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
    unsafe {
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let element = automation.ElementFromHandle(hwnd).ok()?;
        let walker = if control {
            automation.ControlViewWalker()
        } else {
            automation.RawViewWalker()
        }
        .ok()?;
        Some(ProductionUiaNode { element, walker })
    }
}

/// Query committed URL from the first top-level browser Document via UI Automation.
#[cfg(windows)]
pub fn query_browser_document_url(hwnd_val: isize, exe_name: &str) -> Option<String> {
    let lower_exe = exe_name.to_ascii_lowercase();
    if lower_exe != "msedge.exe" && lower_exe != "chrome.exe" {
        return None;
    }

    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return None;
        }
        struct CoUninitGuard;
        impl Drop for CoUninitGuard {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _guard = CoUninitGuard;

        let hwnd = HWND(hwnd_val as *mut _);
        let control_root = uia_root(hwnd, true)?;
        // Reacquire the raw root after metadata-only accessibility initialization.
        walk_uia_views(&control_root, || uia_root(hwnd, false))
    }
}

#[cfg(not(windows))]
pub fn query_browser_document_url(_hwnd_val: isize, _exe_name: &str) -> Option<String> {
    None
}

#[cfg(windows)]
fn helper_identity_matches(window_id: u64, exe_name: &str, expected_title: u64) -> bool {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        let window = GetForegroundWindow();
        if window.0 as u64 != window_id {
            return false;
        }
        let Some(title) = read_window_title(window) else {
            return false;
        };
        if observer_model::desktop_activity::title_fingerprint(&title) != expected_title {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        get_process_image_path(pid)
            .and_then(|image| {
                Path::new(&image)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .is_some_and(|name| name.eq_ignore_ascii_case(exe_name))
    }
}
#[cfg(not(windows))]
fn helper_identity_matches(_: u64, _: &str, _: u64) -> bool {
    false
}

/// Same-binary URL helper entry point dispatched early from main before GUI/mutex.
pub fn run_url_helper_cli() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        println!("UNAVAILABLE");
        return 0;
    }
    let Ok(window_id) = args[2].parse::<isize>() else {
        println!("UNAVAILABLE");
        return 0;
    };
    let exe_name = &args[3];

    let Ok(expected_title) = args[4].parse::<u64>() else {
        println!("UNAVAILABLE");
        return 0;
    };
    let parent_ready = std::env::var("SOLSTONE_PUMP_READY").is_ok_and(|value| value == "1");
    if !parent_ready
        || !is_session_eligible()
        || !helper_identity_matches(window_id as u64, exe_name, expected_title)
    {
        println!("UNAVAILABLE");
        return 0;
    }

    match query_browser_document_url(window_id, exe_name) {
        Some(url) => println!("URL:{url}"),
        None => println!("UNAVAILABLE"),
    }
    0
}

#[cfg(windows)]
#[derive(Debug)]
struct JobHandle(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for JobHandle {}
#[cfg(windows)]
unsafe impl Sync for JobHandle {}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn create_kill_on_close_job() -> Option<JobHandle> {
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_err()
        {
            let _ = windows::Win32::Foundation::CloseHandle(job);
            return None;
        }
        Some(JobHandle(job))
    }
}

/// Contained single worker process management for out-of-process URL querying.
#[derive(Debug, Default)]
pub struct UrlWorker {
    child: Option<Arc<Mutex<Child>>>,
    watchdog_cancel: Option<std::sync::mpsc::Sender<()>>,
    #[cfg(windows)]
    job: Option<Arc<JobHandle>>,
    pending_generation: Option<u64>,
    pending_window_id: Option<u64>,
    spawn_time: Option<Instant>,
    deadline: Option<Instant>,
    killed_by_watchdog: Arc<std::sync::atomic::AtomicBool>,
    reap_failed: bool,
    killed: bool,
}

impl UrlWorker {
    pub fn new() -> Self {
        Self::default()
    }

    fn disarm_watchdog(&mut self) {
        if let Some(tx) = self.watchdog_cancel.take() {
            let _ = tx.send(());
        }
    }

    pub fn is_unreaped(&self) -> bool {
        if let Some(child_arc) = self.child.clone() {
            if let Ok(mut child) = child_arc.lock() {
                return matches!(child.try_wait(), Ok(None));
            }
        }
        false
    }

    pub fn request_program(
        &mut self,
        program: &Path,
        args: &[&str],
        generation: u64,
        window_id: u64,
    ) {
        if self.reap_failed {
            return;
        }

        if let Some(child_arc) = self.child.clone() {
            let mut child = match child_arc.lock() {
                Ok(c) => c,
                Err(_) => {
                    self.reap_failed = true;
                    return;
                }
            };
            match child.try_wait() {
                Ok(Some(_)) => {
                    drop(child);
                    self.disarm_watchdog();
                    self.child = None;
                    #[cfg(windows)]
                    {
                        self.job = None;
                    }
                    self.killed = false;
                    self.pending_generation = None;
                    self.pending_window_id = None;
                    self.spawn_time = None;
                    self.deadline = None;
                }
                Ok(None) => {
                    // Child is still alive and unreaped, do not spawn replacement.
                    return;
                }
                Err(_) => {
                    self.reap_failed = true;
                    return;
                }
            }
        }

        #[cfg(windows)]
        let job = match create_kill_on_close_job() {
            Some(j) => Arc::new(j),
            None => {
                return;
            }
        };

        let spawn_instant = Instant::now();
        let deadline = spawn_instant + Duration::from_secs(2);
        #[cfg(not(windows))]
        let spawned = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        #[cfg(windows)]
        let spawned = Child::spawn(program, args, Arc::clone(&job));
        match spawned {
            Ok(child) => {
                let child_arc = Arc::new(Mutex::new(child));
                let (tx, rx) = std::sync::mpsc::channel();
                let child_for_watchdog = Arc::clone(&child_arc);
                let killed_by_watchdog = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let killed_flag = Arc::clone(&killed_by_watchdog);

                std::thread::spawn(move || {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    if rx.recv_timeout(wait).is_err() {
                        if let Ok(mut c) = child_for_watchdog.lock() {
                            match c.try_wait() {
                                Ok(Some(_)) => {}
                                Ok(None) | Err(_) => {
                                    let _ = c.kill();
                                    killed_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                                }
                            }
                        }
                    }
                });

                #[cfg(windows)]
                if let Ok(mut child) = child_arc.lock() {
                    // The watchdog is armed while the contained process is still suspended.
                    if Instant::now() >= deadline || child.resume().is_err() {
                        let _ = child.kill();
                        killed_by_watchdog.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }

                self.child = Some(child_arc);
                self.watchdog_cancel = Some(tx);
                #[cfg(windows)]
                {
                    self.job = Some(job);
                }
                self.killed = false;
                self.killed_by_watchdog = killed_by_watchdog;
                self.pending_generation = Some(generation);
                self.pending_window_id = Some(window_id);
                self.spawn_time = Some(spawn_instant);
                self.deadline = Some(deadline);
            }
            Err(_) => {
                self.child = None;
                self.watchdog_cancel = None;
                #[cfg(windows)]
                {
                    self.job = None;
                }
                self.killed = false;
                self.pending_generation = None;
                self.pending_window_id = None;
                self.spawn_time = None;
                self.deadline = None;
            }
        }
    }

    pub fn request_url(&mut self, req: DesktopActivityUrlRequest) {
        if !is_session_eligible() || !is_pump_ready() || is_pump_degraded() {
            return;
        }
        if !helper_identity_matches(req.window_id, &req.exe_name, req.title_fingerprint) {
            return;
        }
        let Ok(current_exe) = std::env::current_exe() else {
            return;
        };
        let win_id_str = req.window_id.to_string();
        let title = req.title_fingerprint.to_string();
        let args = ["--desktop-url-helper", &win_id_str, &req.exe_name, &title];
        self.request_program(&current_exe, &args, req.generation, req.window_id);
    }

    pub fn poll_result(&mut self) -> Option<DesktopActivityUrlResult> {
        if self.reap_failed {
            self.disarm_watchdog();
            if let (Some(generation), Some(window_id)) = (
                self.pending_generation.take(),
                self.pending_window_id.take(),
            ) {
                self.spawn_time = None;
                self.deadline = None;
                return Some(DesktopActivityUrlResult {
                    generation,
                    window_id,
                    url: None,
                    unavailable: true,
                });
            }
            return None;
        }

        let child_arc = self.child.clone()?;
        let _spawn_time = self.spawn_time?;

        let mut child = match child_arc.lock() {
            Ok(c) => c,
            Err(_) => {
                self.reap_failed = true;
                return None;
            }
        };

        match child.try_wait() {
            Ok(Some(status)) => {
                let killed_by_watchdog = self
                    .killed_by_watchdog
                    .load(std::sync::atomic::Ordering::SeqCst);
                self.disarm_watchdog();
                let generation = self.pending_generation.take();
                let window_id = self.pending_window_id.take();
                self.spawn_time = None;
                self.deadline = None;

                #[cfg(windows)]
                let timely = child.finished_before_deadline();
                #[cfg(not(windows))]
                let timely =
                    Instant::now().saturating_duration_since(_spawn_time) <= Duration::from_secs(2);
                let result = if !self.killed && !killed_by_watchdog && timely && status.success() {
                    let outcome = if let Some(stdout) = child.stdout.take() {
                        read_bounded_stdout(stdout)
                    } else {
                        UrlHelperOutcome::Unavailable
                    };
                    let (url, unavailable) = match outcome {
                        UrlHelperOutcome::Url(u) => (Some(u), false),
                        UrlHelperOutcome::Unavailable => (None, true),
                    };
                    if let (Some(generation), Some(window_id)) = (generation, window_id) {
                        Some(DesktopActivityUrlResult {
                            generation,
                            window_id,
                            url,
                            unavailable,
                        })
                    } else {
                        None
                    }
                } else if let (Some(generation), Some(window_id)) = (generation, window_id) {
                    Some(DesktopActivityUrlResult {
                        generation,
                        window_id,
                        url: None,
                        unavailable: true,
                    })
                } else {
                    None
                };

                drop(child);
                self.child = None;
                #[cfg(windows)]
                {
                    self.job = None;
                }
                self.killed = false;
                result
            }
            Ok(None) => {
                let past_deadline = match self.deadline {
                    Some(d) => Instant::now() >= d,
                    None => false,
                };
                if past_deadline
                    || self
                        .killed_by_watchdog
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    if child.kill().is_err() {
                        self.reap_failed = true;
                    }
                    self.killed = true;
                    self.disarm_watchdog();
                    if let (Some(generation), Some(window_id)) = (
                        self.pending_generation.take(),
                        self.pending_window_id.take(),
                    ) {
                        self.spawn_time = None;
                        self.deadline = None;
                        return Some(DesktopActivityUrlResult {
                            generation,
                            window_id,
                            url: None,
                            unavailable: true,
                        });
                    }
                }
                None
            }
            Err(_) => {
                self.disarm_watchdog();
                self.reap_failed = true;
                if let (Some(generation), Some(window_id)) = (
                    self.pending_generation.take(),
                    self.pending_window_id.take(),
                ) {
                    self.spawn_time = None;
                    self.deadline = None;
                    return Some(DesktopActivityUrlResult {
                        generation,
                        window_id,
                        url: None,
                        unavailable: true,
                    });
                }
                None
            }
        }
    }

    pub fn cancel(&mut self) {
        self.disarm_watchdog();
        let child_arc = self.child.clone();
        if let Some(child_arc) = child_arc {
            if let Ok(mut child) = child_arc.lock() {
                if !self.killed {
                    if child.kill().is_err() {
                        self.reap_failed = true;
                    } else {
                        self.killed = true;
                    }
                }
                match child.try_wait() {
                    Ok(Some(_)) => {
                        drop(child);
                        self.child = None;
                        #[cfg(windows)]
                        {
                            self.job = None;
                        }
                        self.killed = false;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        self.reap_failed = true;
                    }
                }
            } else {
                self.reap_failed = true;
            }
        }
        self.pending_generation = None;
        self.pending_window_id = None;
        self.spawn_time = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UrlHelperOutcome {
    Url(String),
    Unavailable,
}

pub fn parse_url_helper_output(bytes: &[u8]) -> UrlHelperOutcome {
    if bytes.len() > 4096 {
        return UrlHelperOutcome::Unavailable;
    }
    let newline_pos = match bytes.iter().position(|&b| b == b'\n') {
        Some(pos) => pos,
        None => return UrlHelperOutcome::Unavailable,
    };
    let line_bytes = &bytes[..newline_pos];
    let line = match std::str::from_utf8(line_bytes) {
        Ok(s) => s.trim_end_matches('\r').trim(),
        Err(_) => return UrlHelperOutcome::Unavailable,
    };

    if line == "UNAVAILABLE" {
        UrlHelperOutcome::Unavailable
    } else if let Some(url) = line.strip_prefix("URL:") {
        let trimmed_url = url.trim();
        if trimmed_url.is_empty() {
            UrlHelperOutcome::Unavailable
        } else {
            UrlHelperOutcome::Url(trimmed_url.to_string())
        }
    } else {
        UrlHelperOutcome::Unavailable
    }
}

#[cfg(windows)]
pub fn read_bounded_stdout<R: std::os::windows::io::AsRawHandle + std::io::Read>(
    mut reader: R,
) -> UrlHelperOutcome {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Pipes::PeekNamedPipe;

    let handle = HANDLE(reader.as_raw_handle() as _);
    let start = Instant::now();

    let available;
    loop {
        let mut avail = 0u32;
        let peek_res = unsafe { PeekNamedPipe(handle, None, 0, None, Some(&mut avail), None) };
        if peek_res.is_err() {
            return UrlHelperOutcome::Unavailable;
        }
        if avail > 0 {
            available = avail;
            break;
        }
        if start.elapsed() >= Duration::from_millis(50) {
            return UrlHelperOutcome::Unavailable;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    if available > 4096 {
        return UrlHelperOutcome::Unavailable;
    }

    let to_read = available as usize;
    let mut buf = vec![0u8; to_read];
    if reader.read_exact(&mut buf).is_err() {
        return UrlHelperOutcome::Unavailable;
    }

    parse_url_helper_output(&buf)
}

#[cfg(not(windows))]
fn set_unix_nonblocking(fd: std::os::unix::io::RawFd) -> std::io::Result<()> {
    extern "C" {
        fn fcntl(fd: std::os::raw::c_int, cmd: std::os::raw::c_int, ...) -> std::os::raw::c_int;
    }
    const F_GETFL: std::os::raw::c_int = 3;
    const F_SETFL: std::os::raw::c_int = 4;
    const O_NONBLOCK: std::os::raw::c_int = 0o4000;

    unsafe {
        let flags = fcntl(fd, F_GETFL);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn read_bounded_stdout<R: std::os::unix::io::AsRawFd + std::io::Read>(
    mut reader: R,
) -> UrlHelperOutcome {
    use std::io::ErrorKind;

    let fd = reader.as_raw_fd();
    if set_unix_nonblocking(fd).is_err() {
        return UrlHelperOutcome::Unavailable;
    }

    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    let start = Instant::now();

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 4096 {
                    return UrlHelperOutcome::Unavailable;
                }
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if start.elapsed() >= Duration::from_millis(50) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return UrlHelperOutcome::Unavailable,
        }
    }

    parse_url_helper_output(&buf)
}

#[cfg(windows)]
static DIRTY_FOREGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(windows)]
static DIRTY_TITLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
const OBJID_WINDOW: i32 = 0;
#[cfg(windows)]
const CHILDID_SELF: i32 = 0;

#[cfg(windows)]
unsafe extern "system" fn win_event_proc(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    _hwnd: windows::Win32::Foundation::HWND,
    id_object: i32,
    id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if event == windows::Win32::UI::WindowsAndMessaging::EVENT_SYSTEM_FOREGROUND {
        DIRTY_FOREGROUND.store(true, std::sync::atomic::Ordering::SeqCst);
    } else if event == windows::Win32::UI::WindowsAndMessaging::EVENT_OBJECT_NAMECHANGE
        && id_object == OBJID_WINDOW
        && id_child == CHILDID_SELF
    {
        DIRTY_TITLE.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(windows)]
struct HookThread {
    thread_id: Option<u32>,
    join_handle: Option<std::thread::JoinHandle<()>>,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WindowTitleReadResult {
    Complete(String),
    Truncated,
    Error,
}

pub fn evaluate_window_title_read(
    buf: &[u16],
    copied: usize,
    last_error: u32,
) -> WindowTitleReadResult {
    let capacity = buf.len();
    if capacity == 0 {
        return WindowTitleReadResult::Error;
    }
    if copied == 0 {
        if last_error == 0 {
            WindowTitleReadResult::Complete(String::new())
        } else {
            WindowTitleReadResult::Error
        }
    } else if copied >= capacity - 1 {
        WindowTitleReadResult::Truncated
    } else {
        WindowTitleReadResult::Complete(String::from_utf16_lossy(&buf[..copied]))
    }
}

#[cfg(windows)]
fn read_window_title(hwnd: windows::Win32::Foundation::HWND) -> Option<String> {
    unsafe {
        let mut buf = vec![0u16; 512];
        windows::Win32::Foundation::SetLastError(windows::Win32::Foundation::WIN32_ERROR(0));
        let copied =
            windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buf) as usize;
        let last_err = windows::Win32::Foundation::GetLastError().0;
        match evaluate_window_title_read(&buf, copied, last_err) {
            WindowTitleReadResult::Complete(title) => Some(title),
            WindowTitleReadResult::Error => None,
            WindowTitleReadResult::Truncated => {
                let mut buf = vec![0u16; 2048];
                windows::Win32::Foundation::SetLastError(windows::Win32::Foundation::WIN32_ERROR(
                    0,
                ));
                let copied = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut buf)
                    as usize;
                let last_err = windows::Win32::Foundation::GetLastError().0;
                match evaluate_window_title_read(&buf, copied, last_err) {
                    WindowTitleReadResult::Complete(title) => Some(title),
                    WindowTitleReadResult::Truncated | WindowTitleReadResult::Error => None,
                }
            }
        }
    }
}

/// Real Windows desktop activity probe.
pub struct DesktopActivityProbe {
    state: SourceState,
    #[cfg_attr(not(windows), allow(dead_code))]
    current_window_id: Option<u64>,
    #[cfg_attr(not(windows), allow(dead_code))]
    current_activity_state: Option<ActivityState>,
    #[cfg_attr(not(windows), allow(dead_code))]
    last_input_check: Option<Instant>,
    url_worker: UrlWorker,
    #[cfg(windows)]
    hook_thread: Option<HookThread>,
    #[cfg_attr(not(windows), allow(dead_code))]
    thread_stuck: bool,
    #[cfg_attr(not(windows), allow(dead_code))]
    was_ineligible: bool,
}

impl Default for DesktopActivityProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopActivityProbe {
    pub fn new() -> Self {
        #[cfg(windows)]
        let state = if !is_session_eligible() || is_pump_degraded() {
            SourceState::Faulted {
                reason: ErrorReason::Unknown,
                detail: String::new(),
            }
        } else {
            SourceState::Inactive
        };
        #[cfg(not(windows))]
        let state = SourceState::Inactive;

        Self {
            state,
            current_window_id: None,
            current_activity_state: None,
            last_input_check: None,
            url_worker: UrlWorker::new(),
            #[cfg(windows)]
            hook_thread: None,
            thread_stuck: false,
            was_ineligible: false,
        }
    }
}

impl DesktopActivityProbe {
    fn poll_native_events(&mut self, include_content: bool) -> Vec<DesktopActivityProbeEvent> {
        #[cfg(not(windows))]
        let _ = include_content;
        if self.state != SourceState::Active {
            return Vec::new();
        }

        #[cfg(windows)]
        {
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

            let mut events = Vec::new();

            if !is_session_eligible() || !is_pump_ready() || is_pump_degraded() {
                self.was_ineligible = true;
                self.url_worker.cancel();
                DIRTY_FOREGROUND.store(false, std::sync::atomic::Ordering::SeqCst);
                DIRTY_TITLE.store(false, std::sync::atomic::Ordering::SeqCst);
                self.current_window_id = None;
                if self.current_activity_state != Some(ActivityState::Locked) {
                    self.current_activity_state = Some(ActivityState::Locked);
                    events.push(DesktopActivityProbeEvent::ActivityStateChanged(
                        ActivityState::Locked,
                    ));
                }
                return events;
            }

            let was_ineligible = self.was_ineligible;
            self.was_ineligible = false;

            // Idle timer check (5s cadence, 60s idle threshold)
            let now = Instant::now();
            let check_idle = match self.last_input_check {
                None => true,
                Some(last) => now.duration_since(last) >= Duration::from_secs(5),
            };

            if check_idle {
                self.last_input_check = Some(now);
                unsafe {
                    let mut lii = LASTINPUTINFO {
                        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                        dwTime: 0,
                    };
                    if GetLastInputInfo(&mut lii).as_bool() {
                        let tick_count = windows::Win32::System::SystemInformation::GetTickCount();
                        let idle_ms = tick_count.wrapping_sub(lii.dwTime);
                        let new_state = if idle_ms >= 60_000 {
                            ActivityState::Idle
                        } else {
                            ActivityState::Active
                        };
                        if self.current_activity_state != Some(new_state) {
                            self.current_activity_state = Some(new_state);
                            events.push(DesktopActivityProbeEvent::ActivityStateChanged(new_state));
                        }
                    } else {
                        self.current_activity_state = None;
                        events.push(DesktopActivityProbeEvent::IdleUnavailable);
                    }
                }
            }

            if !include_content {
                return events;
            }

            let dirty_fg = DIRTY_FOREGROUND.swap(false, std::sync::atomic::Ordering::SeqCst);
            let dirty_title = DIRTY_TITLE.swap(false, std::sync::atomic::Ordering::SeqCst);

            if dirty_fg || dirty_title || was_ineligible {
                let fg_hwnd =
                    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
                if fg_hwnd.0.is_null() {
                    if self.current_window_id.is_some() {
                        self.current_window_id = None;
                        events.push(DesktopActivityProbeEvent::ForegroundUnavailable);
                    }
                } else {
                    let window_id = fg_hwnd.0 as u64;
                    let title = match read_window_title(fg_hwnd) {
                        Some(t) => t,
                        None => {
                            self.current_window_id = None;
                            events.push(DesktopActivityProbeEvent::ForegroundUnavailable);
                            return events;
                        }
                    };

                    let mut pid = 0u32;
                    unsafe {
                        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                            fg_hwnd,
                            Some(&mut pid),
                        );
                    }

                    let exe_path = get_process_image_path(pid);
                    let (app, exe_name) = match exe_path {
                        Some(ref path_str) => {
                            let path = Path::new(path_str);
                            let exe_n = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            let file_desc = get_file_description(path_str);
                            let app_name = file_desc.unwrap_or_else(|| exe_n.clone());
                            (app_name, exe_n)
                        }
                        None => {
                            self.current_window_id = None;
                            events.push(DesktopActivityProbeEvent::ForegroundUnavailable);
                            return events;
                        }
                    };

                    self.current_window_id = Some(window_id);
                    events.push(DesktopActivityProbeEvent::Foreground {
                        window_id,
                        app,
                        exe_name,
                        title,
                    });
                }
            }

            events
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }
}

impl DesktopActivitySource for DesktopActivityProbe {
    fn start(&mut self) -> Result<(), SourceError> {
        #[cfg(windows)]
        {
            if let Some(thread) = &mut self.hook_thread {
                if thread
                    .join_handle
                    .as_ref()
                    .is_some_and(|handle| handle.is_finished())
                {
                    let _ = thread.join_handle.take().unwrap().join();
                    self.hook_thread = None;
                    self.thread_stuck = false;
                }
            }
            if self.hook_thread.is_none() {
                self.thread_stuck = false;
            }
            if self.thread_stuck || !is_session_eligible() || is_pump_degraded() || !is_pump_ready()
            {
                self.state = SourceState::Faulted {
                    reason: ErrorReason::Unknown,
                    detail: String::new(),
                };
                return Ok(());
            }

            if let Some(thread) = &mut self.hook_thread {
                if let Some(handle) = thread.join_handle.as_ref() {
                    if handle.is_finished() {
                        let _ = thread.join_handle.take().unwrap().join();
                        self.hook_thread = None;
                    } else {
                        if thread.shutdown.load(std::sync::atomic::Ordering::SeqCst) {
                            self.state = SourceState::Faulted {
                                reason: ErrorReason::Unknown,
                                detail: String::new(),
                            };
                            return Ok(());
                        }
                        DIRTY_FOREGROUND.store(true, std::sync::atomic::Ordering::SeqCst);
                        self.last_input_check = None;
                        self.current_activity_state = None;
                        self.state = SourceState::Active;
                        return Ok(());
                    }
                }
            }

            let published_thread_id = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
            let published_thread_id_clone = published_thread_id.clone();
            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let shutdown_clone = shutdown.clone();

            let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<u32, ()>>();

            let join_handle = std::thread::spawn(move || unsafe {
                let thread_id = windows::Win32::System::Threading::GetCurrentThreadId();
                published_thread_id_clone.store(thread_id, std::sync::atomic::Ordering::SeqCst);

                let hook1 = windows::Win32::UI::Accessibility::SetWinEventHook(
                    windows::Win32::UI::WindowsAndMessaging::EVENT_SYSTEM_FOREGROUND,
                    windows::Win32::UI::WindowsAndMessaging::EVENT_SYSTEM_FOREGROUND,
                    windows::Win32::Foundation::HMODULE::default(),
                    Some(win_event_proc),
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::WINEVENT_OUTOFCONTEXT,
                );
                let hook2 = windows::Win32::UI::Accessibility::SetWinEventHook(
                    windows::Win32::UI::WindowsAndMessaging::EVENT_OBJECT_NAMECHANGE,
                    windows::Win32::UI::WindowsAndMessaging::EVENT_OBJECT_NAMECHANGE,
                    windows::Win32::Foundation::HMODULE::default(),
                    Some(win_event_proc),
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::WINEVENT_OUTOFCONTEXT,
                );

                if hook1.is_invalid() || hook2.is_invalid() {
                    if !hook1.is_invalid() {
                        let _ = windows::Win32::UI::Accessibility::UnhookWinEvent(hook1);
                    }
                    if !hook2.is_invalid() {
                        let _ = windows::Win32::UI::Accessibility::UnhookWinEvent(hook2);
                    }
                    let _ = ready_tx.send(Err(()));
                    return;
                }

                let _ = ready_tx.send(Ok(thread_id));

                let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
                loop {
                    if shutdown_clone.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let res = windows::Win32::UI::WindowsAndMessaging::GetMessageW(
                        &mut msg,
                        windows::Win32::Foundation::HWND::default(),
                        0,
                        0,
                    );
                    if res.0 == 0 || res.0 == -1 {
                        break;
                    }
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                    windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
                }

                let _ = windows::Win32::UI::Accessibility::UnhookWinEvent(hook1);
                let _ = windows::Win32::UI::Accessibility::UnhookWinEvent(hook2);
            });

            match ready_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Ok(thread_id)) => {
                    self.hook_thread = Some(HookThread {
                        thread_id: Some(thread_id),
                        join_handle: Some(join_handle),
                        shutdown,
                    });
                    self.state = SourceState::Active;
                    self.last_input_check = Some(Instant::now());

                    DIRTY_FOREGROUND.store(true, std::sync::atomic::Ordering::SeqCst);

                    Ok(())
                }
                _ => {
                    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
                    let tid = published_thread_id.load(std::sync::atomic::Ordering::SeqCst);
                    if tid != 0 {
                        unsafe {
                            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                                tid,
                                windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                                windows::Win32::Foundation::WPARAM(0),
                                windows::Win32::Foundation::LPARAM(0),
                            );
                        }
                    }
                    let finished = join_handle.is_finished();
                    self.hook_thread = Some(HookThread {
                        thread_id: if tid != 0 { Some(tid) } else { None },
                        join_handle: Some(join_handle),
                        shutdown,
                    });
                    if !finished {
                        self.thread_stuck = true;
                    }
                    self.state = SourceState::Faulted {
                        reason: ErrorReason::Unknown,
                        detail: String::new(),
                    };
                    Ok(())
                }
            }
        }
        #[cfg(not(windows))]
        {
            self.state = SourceState::Inactive;
            Ok(())
        }
    }

    fn stop(&mut self) {
        self.url_worker.cancel();
        #[cfg(windows)]
        {
            if let Some(thread) = &mut self.hook_thread {
                thread
                    .shutdown
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                if let Some(tid) = thread.thread_id {
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                            tid,
                            windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                            windows::Win32::Foundation::WPARAM(0),
                            windows::Win32::Foundation::LPARAM(0),
                        );
                    }
                }
                let mut joined = false;
                if let Some(handle) = thread.join_handle.as_ref() {
                    for _ in 0..50 {
                        if handle.is_finished() {
                            joined = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                if joined {
                    let _ = thread.join_handle.take().unwrap().join();
                    self.hook_thread = None;
                } else {
                    self.thread_stuck = true;
                    self.state = SourceState::Faulted {
                        reason: ErrorReason::Unknown,
                        detail: String::new(),
                    };
                    return;
                }
            }
        }
        self.state = SourceState::Inactive;
        self.current_window_id = None;
        self.current_activity_state = None;
    }

    fn state(&self) -> SourceState {
        #[cfg(windows)]
        if !is_session_eligible() || !is_pump_ready() || is_pump_degraded() {
            return SourceState::Faulted {
                reason: ErrorReason::Unknown,
                detail: String::new(),
            };
        }
        self.state.clone()
    }

    fn poll_events(&mut self) -> Vec<DesktopActivityProbeEvent> {
        self.poll_native_events(true)
    }
    fn poll_coarse_events(&mut self) -> Vec<DesktopActivityProbeEvent> {
        self.poll_native_events(false)
    }

    fn request_url(&mut self, request: DesktopActivityUrlRequest) {
        self.url_worker.request_url(request);
    }

    fn poll_url_result(&mut self) -> Option<DesktopActivityUrlResult> {
        self.url_worker.poll_result()
    }

    fn cancel_url(&mut self) {
        self.url_worker.cancel();
    }

    fn foreground_snapshot(
        &mut self,
    ) -> Option<observer_model::desktop_activity::ForegroundSnapshot> {
        #[cfg(windows)]
        {
            if self.state != SourceState::Active
                || !is_session_eligible()
                || !is_pump_ready()
                || is_pump_degraded()
            {
                return None;
            }
            let fg_hwnd = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            if fg_hwnd.0.is_null() {
                return None;
            }
            let window_id = fg_hwnd.0 as u64;
            let title = read_window_title(fg_hwnd)?;
            let mut pid = 0u32;
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                    fg_hwnd,
                    Some(&mut pid),
                );
            }
            let exe_path = get_process_image_path(pid)?;
            let path = Path::new(&exe_path);
            let exe_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let file_desc = get_file_description(&exe_path);
            let app = file_desc.unwrap_or_else(|| exe_name.clone());
            Some(observer_model::desktop_activity::ForegroundSnapshot {
                window_id,
                app,
                exe_name,
                title,
            })
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

#[cfg(windows)]
fn get_process_image_path(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let success = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        if success.is_ok() && size > 0 {
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct MockArena {
        nodes: Vec<MockNodeData>,
        value_reads: usize,
        next_sibling_calls: usize,
        first_child_calls: usize,
        navigation_error: Option<(usize, bool)>,
        node_acquisitions: usize,
    }

    #[derive(Clone)]
    struct MockNodeData {
        control_type: Result<i32, UiaFactUnavailable>,
        is_password: Result<bool, UiaFactUnavailable>,
        is_control: Result<bool, UiaFactUnavailable>,
        value: Option<String>,
        legacy_value: Option<String>,
        first_child: Option<usize>,
        next_sibling: Option<usize>,
    }

    #[derive(Clone)]
    struct MockNode {
        id: usize,
        arena: Arc<Mutex<MockArena>>,
    }

    impl UiaNode for MockNode {
        fn control_type(&self) -> Result<i32, UiaFactUnavailable> {
            let arena = self.arena.lock().unwrap();
            arena.nodes[self.id].control_type
        }

        fn is_password(&self) -> Result<bool, UiaFactUnavailable> {
            let arena = self.arena.lock().unwrap();
            arena.nodes[self.id].is_password
        }

        fn is_control_element(&self) -> Result<bool, UiaFactUnavailable> {
            self.arena.lock().unwrap().nodes[self.id].is_control
        }

        fn value_property(&self) -> Option<String> {
            let mut arena = self.arena.lock().unwrap();
            arena.value_reads += 1;
            arena.nodes[self.id].value.clone()
        }

        fn legacy_value_property(&self) -> Option<String> {
            let arena = self.arena.lock().unwrap();
            arena.nodes[self.id].legacy_value.clone()
        }

        fn first_child(&self) -> Result<Option<Self>, UiaFactUnavailable> {
            let mut arena = self.arena.lock().unwrap();
            arena.first_child_calls += 1;
            if arena.navigation_error == Some((self.id, true)) {
                return Err(UiaFactUnavailable);
            }
            if arena.nodes[self.id].first_child.is_some() {
                arena.node_acquisitions += 1;
            }
            Ok(arena.nodes[self.id].first_child.map(|id| MockNode {
                id,
                arena: self.arena.clone(),
            }))
        }

        fn next_sibling(&self) -> Result<Option<Self>, UiaFactUnavailable> {
            let mut arena = self.arena.lock().unwrap();
            arena.next_sibling_calls += 1;
            if arena.navigation_error == Some((self.id, false)) {
                return Err(UiaFactUnavailable);
            }
            if arena.nodes[self.id].next_sibling.is_some() {
                arena.node_acquisitions += 1;
            }
            Ok(arena.nodes[self.id].next_sibling.map(|id| MockNode {
                id,
                arena: self.arena.clone(),
            }))
        }
    }

    #[test]
    fn non_control_document_is_skipped_without_value_reads_or_descending() {
        let node = |control_type, is_control, value: Option<&str>, first_child, next_sibling| {
            MockNodeData {
                control_type: Ok(control_type),
                is_password: Ok(false),
                is_control: Ok(is_control),
                value: value.map(str::to_owned),
                legacy_value: None,
                first_child,
                next_sibling,
            }
        };
        let arena = Arc::new(Mutex::new(MockArena {
            nodes: vec![
                node(50000, true, None, Some(1), None),
                node(
                    50030,
                    false,
                    Some("https://invalid.test/non-control"),
                    Some(3),
                    Some(2),
                ),
                node(
                    50030,
                    true,
                    Some("https://example.test/committed?secret=1#fragment"),
                    None,
                    None,
                ),
                node(50030, true, Some("https://invalid.test/nested"), None, None),
            ],
            value_reads: 0,
            next_sibling_calls: 0,
            first_child_calls: 0,
            navigation_error: None,
            node_acquisitions: 0,
        }));
        let root = MockNode {
            id: 0,
            arena: Arc::clone(&arena),
        };
        assert_eq!(
            walk_uia_tree(&root).as_deref(),
            Some("https://example.test/committed")
        );
        let locked = arena.lock().unwrap();
        assert_eq!(locked.value_reads, 1);
        assert_eq!(locked.first_child_calls, 1);
    }

    fn mock_tree(nodes: Vec<MockNodeData>) -> MockNode {
        MockNode {
            id: 0,
            arena: Arc::new(Mutex::new(MockArena {
                nodes,
                value_reads: 0,
                next_sibling_calls: 0,
                first_child_calls: 0,
                navigation_error: None,
                node_acquisitions: 0,
            })),
        }
    }

    fn mock_fact(control_type: i32, value: Option<&str>) -> MockNodeData {
        MockNodeData {
            control_type: Ok(control_type),
            is_password: Ok(false),
            is_control: Ok(true),
            value: value.map(str::to_owned),
            legacy_value: None,
            first_child: None,
            next_sibling: None,
        }
    }

    #[test]
    fn warmup_reads_no_values_and_raw_pass_still_checks_secure_ancestors() {
        let control = mock_tree(vec![mock_fact(50030, Some("https://invalid.test/warmup"))]);
        let raw = mock_tree(vec![mock_fact(
            50030,
            Some("https://example.test/committed?q=secret"),
        )]);
        assert_eq!(
            walk_uia_views(&control, || Some(raw.clone())).as_deref(),
            Some("https://example.test/committed")
        );
        assert_eq!(control.arena.lock().unwrap().value_reads, 0);
        assert_eq!(raw.arena.lock().unwrap().value_reads, 1);

        let mut secure_parent = mock_fact(50000, None);
        secure_parent.is_password = Ok(true);
        secure_parent.first_child = Some(1);
        let raw = mock_tree(vec![
            secure_parent,
            mock_fact(50030, Some("https://invalid.test/secure")),
        ]);
        assert_eq!(walk_uia_views(&control, || Some(raw.clone())), None);
        assert_eq!(raw.arena.lock().unwrap().value_reads, 0);
        assert_eq!(raw.arena.lock().unwrap().first_child_calls, 0);
    }

    #[test]
    fn completed_warmup_without_document_allows_raw_but_limits_are_shared() {
        let control = mock_tree(vec![mock_fact(50000, None)]);
        let raw = mock_tree(vec![mock_fact(
            50030,
            Some("https://example.test/committed"),
        )]);
        assert_eq!(
            walk_uia_views(&control, || Some(raw.clone())).as_deref(),
            Some("https://example.test/committed")
        );

        let mut nodes = vec![mock_fact(50000, None)];
        nodes[0].first_child = Some(1);
        for i in 1..400 {
            let mut node = mock_fact(50004, Some("https://invalid.test/edit"));
            if i < 399 {
                node.next_sibling = Some(i + 1);
            }
            nodes.push(node);
        }
        let control = mock_tree(nodes);
        let raw = mock_tree(vec![mock_fact(
            50030,
            Some("https://invalid.test/over-budget"),
        )]);
        let acquired = std::cell::Cell::new(false);
        assert_eq!(
            walk_uia_views(&control, || {
                acquired.set(true);
                Some(raw.clone())
            }),
            None
        );
        assert!(
            !acquired.get(),
            "exhausted warmup must not acquire the RAW root"
        );
        let control_arena = control.arena.lock().unwrap();
        assert_eq!(
            control_arena.node_acquisitions, 399,
            "plus the control root is exactly400 elements"
        );
        assert_eq!(control_arena.value_reads, 0);
        assert_eq!(raw.arena.lock().unwrap().value_reads, 0);
    }

    #[test]
    fn unknown_control_document_fact_stops_before_value_or_later_document() {
        let mut first = mock_fact(50030, Some("https://invalid.test/unknown"));
        first.is_control = Err(UiaFactUnavailable);
        first.next_sibling = Some(2);
        let mut root = mock_fact(50000, None);
        root.first_child = Some(1);
        let raw = mock_tree(vec![
            root,
            first,
            mock_fact(50030, Some("https://invalid.test/later")),
        ]);
        assert_eq!(walk_uia_tree(&raw), None);
        assert_eq!(raw.arena.lock().unwrap().value_reads, 0);
    }

    #[test]
    fn navigation_failure_never_skips_an_unread_branch_for_a_later_document() {
        let mut root = mock_fact(50000, None);
        root.first_child = Some(1);
        let mut branch = mock_fact(50000, None);
        branch.next_sibling = Some(2);
        let raw = mock_tree(vec![
            root,
            branch,
            mock_fact(50030, Some("https://invalid.test/later")),
        ]);
        for failure in [(1, true), (1, false)] {
            raw.arena.lock().unwrap().navigation_error = Some(failure);
            assert_eq!(walk_uia_tree(&raw), None);
            assert_eq!(raw.arena.lock().unwrap().value_reads, 0);
        }
    }

    #[test]
    fn test_500_siblings_stop_by_400_and_do_not_read_the_rest() {
        let mut nodes = Vec::new();

        // Node 0: root
        nodes.push(MockNodeData {
            control_type: Ok(50000), // Group
            is_password: Ok(false),
            is_control: Ok(true),
            value: None,
            legacy_value: None,
            first_child: Some(1),
            next_sibling: None,
        });

        // 500 siblings
        for i in 1..=500 {
            let next = if i < 500 { Some(i + 1) } else { None };
            nodes.push(MockNodeData {
                control_type: Ok(50004), // Edit box
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: None,
                next_sibling: next,
            });
        }

        let arena = Arc::new(Mutex::new(MockArena {
            nodes,
            value_reads: 0,
            next_sibling_calls: 0,
            first_child_calls: 0,
            navigation_error: None,
            node_acquisitions: 0,
        }));

        let root = MockNode {
            id: 0,
            arena: arena.clone(),
        };

        let result = walk_uia_tree(&root);
        assert_eq!(result, None);

        let calls = arena.lock().unwrap().next_sibling_calls;
        assert!(calls <= 400, "expected <= 400 sibling calls, got {calls}");
        assert!(calls < 500, "expected fewer than 500 calls, got {calls}");
    }

    #[test]
    fn test_password_node_does_not_get_value_read_and_children_not_walked() {
        let nodes = vec![
            // Node 0: root
            MockNodeData {
                control_type: Ok(50000),
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: Some(1),
                next_sibling: None,
            },
            // Node 1: Password field with Document child
            MockNodeData {
                control_type: Ok(50030), // Document
                is_password: Ok(true),   // Password!
                is_control: Ok(true),
                value: Some("https://password-leak.com".into()),
                legacy_value: None,
                first_child: Some(2),
                next_sibling: None,
            },
            // Node 2: Child of password node
            MockNodeData {
                control_type: Ok(50030),
                is_password: Ok(false),
                is_control: Ok(true),
                value: Some("https://child-doc.com".into()),
                legacy_value: None,
                first_child: None,
                next_sibling: None,
            },
        ];

        let arena = Arc::new(Mutex::new(MockArena {
            nodes,
            value_reads: 0,
            next_sibling_calls: 0,
            first_child_calls: 0,
            navigation_error: None,
            node_acquisitions: 0,
        }));

        let root = MockNode {
            id: 0,
            arena: arena.clone(),
        };

        let result = walk_uia_tree(&root);
        assert_eq!(result, None);

        let lock = arena.lock().unwrap();
        assert_eq!(lock.value_reads, 0, "password value must never be read");
        assert_eq!(
            lock.first_child_calls, 1,
            "children of password node must not be walked"
        );
    }

    #[test]
    fn test_focused_and_blurred_address_edit_box_drafts_are_not_the_result() {
        let nodes = vec![
            // Node 0: root
            MockNodeData {
                control_type: Ok(50000),
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: Some(1),
                next_sibling: None,
            },
            // Node 1: addressEditBox (Edit ControlType 50004) with draft URL
            MockNodeData {
                control_type: Ok(50004),
                is_password: Ok(false),
                is_control: Ok(true),
                value: Some("https://draft-in-address-bar.com/draft".into()),
                legacy_value: Some("https://draft-in-address-bar.com/draft".into()),
                first_child: None,
                next_sibling: Some(2),
            },
            // Node 2: Document (ControlType 50030) with committed URL
            MockNodeData {
                control_type: Ok(50030),
                is_password: Ok(false),
                is_control: Ok(true),
                value: Some("https://committed.example.com/page?query#frag".into()),
                legacy_value: Some("https://committed.example.com/page?query#frag".into()),
                first_child: None,
                next_sibling: None,
            },
        ];

        let arena = Arc::new(Mutex::new(MockArena {
            nodes,
            value_reads: 0,
            next_sibling_calls: 0,
            first_child_calls: 0,
            navigation_error: None,
            node_acquisitions: 0,
        }));

        let root = MockNode {
            id: 0,
            arena: arena.clone(),
        };

        let result = walk_uia_tree(&root);
        assert_eq!(result, Some("https://committed.example.com/page".into()));
    }

    #[test]
    fn test_document_value_admitted_when_present_and_withheld_when_absent() {
        // 1. Present
        let nodes1 = vec![
            MockNodeData {
                control_type: Ok(50000),
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: Some(1),
                next_sibling: None,
            },
            MockNodeData {
                control_type: Ok(50030),
                is_password: Ok(false),
                is_control: Ok(true),
                value: Some("https://example.com/foo".into()),
                legacy_value: None,
                first_child: None,
                next_sibling: None,
            },
        ];
        let root1 = MockNode {
            id: 0,
            arena: Arc::new(Mutex::new(MockArena {
                nodes: nodes1,
                value_reads: 0,
                next_sibling_calls: 0,
                first_child_calls: 0,
                navigation_error: None,
                node_acquisitions: 0,
            })),
        };
        assert_eq!(
            walk_uia_tree(&root1),
            Some("https://example.com/foo".into())
        );

        // 2. Absent (no value or legacy value)
        let nodes2 = vec![
            MockNodeData {
                control_type: Ok(50000),
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: Some(1),
                next_sibling: None,
            },
            MockNodeData {
                control_type: Ok(50030),
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: None,
                next_sibling: None,
            },
        ];
        let root2 = MockNode {
            id: 0,
            arena: Arc::new(Mutex::new(MockArena {
                nodes: nodes2,
                value_reads: 0,
                next_sibling_calls: 0,
                first_child_calls: 0,
                navigation_error: None,
                node_acquisitions: 0,
            })),
        };
        assert_eq!(walk_uia_tree(&root2), None);

        // 3. Withheld when both values normalize and differ
        let nodes3 = vec![
            MockNodeData {
                control_type: Ok(50000),
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: Some(1),
                next_sibling: None,
            },
            MockNodeData {
                control_type: Ok(50030),
                is_password: Ok(false),
                is_control: Ok(true),
                value: Some("https://site-a.com/".into()),
                legacy_value: Some("https://site-b.com/".into()),
                first_child: None,
                next_sibling: None,
            },
        ];
        let root3 = MockNode {
            id: 0,
            arena: Arc::new(Mutex::new(MockArena {
                nodes: nodes3,
                value_reads: 0,
                next_sibling_calls: 0,
                first_child_calls: 0,
                navigation_error: None,
                node_acquisitions: 0,
            })),
        };
        assert_eq!(walk_uia_tree(&root3), None);
    }

    #[test]
    fn test_two_document_siblings_first_password_stops_walk_second_not_read() {
        let nodes = vec![
            // Node 0: root
            MockNodeData {
                control_type: Ok(50000), // Group
                is_password: Ok(false),
                is_control: Ok(true),
                value: None,
                legacy_value: None,
                first_child: Some(1),
                next_sibling: None,
            },
            // Node 1: First Document (ControlType 50030), is_password Ok(true)
            MockNodeData {
                control_type: Ok(50030), // Document
                is_password: Ok(true),   // Secure password node
                is_control: Ok(true),
                value: Some("https://should-not-read-pass.com".into()),
                legacy_value: None,
                first_child: None,
                next_sibling: Some(2),
            },
            // Node 2: Second Document (ControlType 50030), is_password Ok(false), has valid URL
            MockNodeData {
                control_type: Ok(50030), // Document
                is_password: Ok(false),
                is_control: Ok(true),
                value: Some("https://example.com/should-not-read".into()),
                legacy_value: Some("https://example.com/should-not-read".into()),
                first_child: None,
                next_sibling: None,
            },
        ];

        let arena = Arc::new(Mutex::new(MockArena {
            nodes,
            value_reads: 0,
            next_sibling_calls: 0,
            first_child_calls: 0,
            navigation_error: None,
            node_acquisitions: 0,
        }));

        let root = MockNode {
            id: 0,
            arena: arena.clone(),
        };

        let result = walk_uia_tree(&root);
        assert_eq!(
            result, None,
            "walk must return None when first document is password"
        );

        let lock = arena.lock().unwrap();
        assert_eq!(
            lock.value_reads, 0,
            "no value reads allowed on either document"
        );
    }

    #[cfg(windows)]
    #[test]
    fn containment_hung_stop_returns_without_waiting() {
        let mut worker = UrlWorker::new();
        let ping = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32/ping.exe");
        worker.request_program(&ping, &["-n", "60", "127.0.0.1"], 1, 123);
        assert!(
            worker.is_unreaped(),
            "must exercise a live contained helper"
        );
        worker.request_program(&ping, &["-n", "60", "127.0.0.1"], 2, 456);
        assert_eq!(
            worker.pending_generation,
            Some(1),
            "a live helper prevents overlap"
        );

        let start = Instant::now();
        worker.cancel();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "cancel took too long: {elapsed:?}"
        );

        let deadline = Instant::now() + Duration::from_millis(500);
        while worker.is_unreaped() {
            assert!(
                Instant::now() < deadline,
                "cancelled helper must retire promptly"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        worker.request_program(&ping, &["-n", "60", "127.0.0.1"], 2, 456);
        assert_eq!(worker.pending_generation, Some(2));
        assert!(!worker.reap_failed);
        worker.cancel();
    }

    #[cfg(windows)]
    #[test]
    fn watchdog_kills_unpolled_child_at_two_seconds() {
        let mut worker = UrlWorker::new();
        let ping = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32/ping.exe");
        worker.request_program(&ping, &["-n", "30", "127.0.0.1"], 1, 100);
        assert!(worker.is_unreaped(), "must start a live contained helper");
        let start = Instant::now();
        let mut exited = false;
        while start.elapsed() < Duration::from_millis(2200) {
            if let Some(child_arc) = worker.child.as_ref() {
                if let Ok(mut c) = child_arc.lock() {
                    if let Ok(Some(_)) = c.try_wait() {
                        exited = true;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            exited,
            "child must have exited within 2.2 seconds without polling"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cancel_after_helper_exit_does_not_poison_future_requests() {
        let ping = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32/ping.exe");
        let mut worker = UrlWorker::new();
        worker.request_program(&ping, &["-n", "1", "127.0.0.1"], 1, 100);
        let child = worker.child.as_ref().expect("spawned helper").clone();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if child.lock().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "helper must exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        worker.cancel();
        assert!(!worker.reap_failed);
        worker.request_program(&ping, &["-n", "60", "127.0.0.1"], 2, 100);
        assert_eq!(worker.pending_generation, Some(2));
        assert!(worker.is_unreaped());
        worker.cancel();
    }

    #[cfg(windows)]
    #[test]
    fn read_bounded_stdout_returns_while_descendant_holds_write_end() {
        let mut child = Command::new("cmd")
            .arg("/c")
            .arg("echo URL:https://example.com/held& start /b ping -n 6 127.0.0.1")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cmd");

        let status = child.wait().expect("wait on cmd parent");
        assert!(status.success());

        let stdout = child.stdout.take().expect("piped stdout");

        let start = Instant::now();
        let outcome = read_bounded_stdout(stdout);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "read_bounded_stdout took too long: {elapsed:?}"
        );
        assert_eq!(
            outcome,
            UrlHelperOutcome::Url("https://example.com/held".to_string())
        );
    }

    #[test]
    fn test_evaluate_window_title_read_empty_and_errors() {
        let buf = [0u16; 512];
        // Empty title with last_error == 0 -> Complete("")
        assert_eq!(
            evaluate_window_title_read(&buf, 0, 0),
            WindowTitleReadResult::Complete(String::new())
        );

        // Empty title with last_error != 0 -> Error
        assert_eq!(
            evaluate_window_title_read(&buf, 0, 123),
            WindowTitleReadResult::Error
        );

        // Zero capacity buffer -> Error
        assert_eq!(
            evaluate_window_title_read(&[], 0, 0),
            WindowTitleReadResult::Error
        );
    }

    #[test]
    fn test_evaluate_window_title_read_truncation_and_success() {
        let mut buf = [0u16; 512];
        buf[..510].fill('a' as u16);

        // Fits comfortably in buffer (copied < capacity - 1)
        assert_eq!(
            evaluate_window_title_read(&buf, 10, 0),
            WindowTitleReadResult::Complete("aaaaaaaaaa".to_string())
        );

        // Fills buffer to capacity - 1 -> Truncated
        assert_eq!(
            evaluate_window_title_read(&buf, 511, 0),
            WindowTitleReadResult::Truncated
        );

        // Fills buffer to capacity -> Truncated
        assert_eq!(
            evaluate_window_title_read(&buf, 512, 0),
            WindowTitleReadResult::Truncated
        );
    }

    #[test]
    fn test_parse_url_helper_output() {
        assert_eq!(
            parse_url_helper_output(b"URL:https://example.com/foo\n"),
            UrlHelperOutcome::Url("https://example.com/foo".to_string())
        );
        assert_eq!(
            parse_url_helper_output(b"URL:https://example.com/foo\r\n"),
            UrlHelperOutcome::Url("https://example.com/foo".to_string())
        );
        assert_eq!(
            parse_url_helper_output(b"UNAVAILABLE\n"),
            UrlHelperOutcome::Unavailable
        );
        assert_eq!(
            parse_url_helper_output(b"UNAVAILABLE\r\n"),
            UrlHelperOutcome::Unavailable
        );
        assert_eq!(
            parse_url_helper_output(b"URL:https://example.com/foo"),
            UrlHelperOutcome::Unavailable
        );
        assert_eq!(
            parse_url_helper_output(b"URL:   \n"),
            UrlHelperOutcome::Unavailable
        );
        assert_eq!(
            parse_url_helper_output(b"unknown output\n"),
            UrlHelperOutcome::Unavailable
        );
        assert_eq!(
            parse_url_helper_output(&vec![b'a'; 4097]),
            UrlHelperOutcome::Unavailable
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_pipe_writer_stays_open_reader_returns_without_blocking() {
        use std::io::Write;
        use std::os::unix::io::FromRawFd;

        extern "C" {
            #[link_name = "pipe"]
            fn libc_pipe(fds: *mut std::os::raw::c_int) -> std::os::raw::c_int;
        }

        let mut fds = [0 as std::os::raw::c_int; 2];
        let ret = unsafe { libc_pipe(fds.as_mut_ptr()) };
        assert_eq!(ret, 0);

        let read_file = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut write_file = unsafe { std::fs::File::from_raw_fd(fds[1]) };

        // Write a complete line but keep write end open
        write_file
            .write_all(b"URL:https://example.com/unclosed_writer\n")
            .unwrap();
        write_file.flush().unwrap();

        let start = Instant::now();
        let outcome = read_bounded_stdout(read_file);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(500),
            "read took too long: {elapsed:?}"
        );
        assert_eq!(
            outcome,
            UrlHelperOutcome::Url("https://example.com/unclosed_writer".to_string())
        );

        drop(write_file);
    }
}
