// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Screen H.264 encoder seam.
//!
//! The Windows implementation owns the Media Foundation sink-writer worker; the
//! non-Windows implementation is an inert compile-time stub.

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::ptr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread::{self, JoinHandle};

    use observer_model::{
        EncoderConfig, EncoderError, EncoderErrorKind, EncoderHealth, ScreenEncoder, ScreenFrame,
        SCREEN_FILE_NAME,
    };
    use windows::core::{Interface, GUID, HRESULT, PCWSTR, VARIANT};
    use windows::Win32::Media::MediaFoundation::{
        eAVEncH264VProfile_High, CODECAPI_AVEncMPVGOPSize, ICodecAPI, IMFAttributes, IMFMediaType,
        IMFSinkWriter, MFCreateAttributes, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
        MFCreateSinkWriterFromURL, MFMediaType_Video, MFShutdown, MFStartup,
        MFTranscodeContainerType_MPEG4, MFVideoFormat_H264, MFVideoFormat_NV12,
        MFVideoInterlace_Progressive, MFSTARTUP_FULL, MF_E_DXGI_DEVICE_NOT_INITIALIZED,
        MF_E_DXGI_NEW_VIDEO_DEVICE, MF_E_HW_MFT_FAILED_START_STREAMING, MF_E_NEW_VIDEO_DEVICE,
        MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
        MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
        MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SINK_WRITER_DISABLE_THROTTLING,
        MF_TRANSCODE_CONTAINERTYPE, MF_VERSION,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    const FRAME_DURATION_100NS: i64 = 10_000_000;
    const PARTIAL_SUFFIX: &str = ".partial";
    const DXGI_ERROR_DEVICE_REMOVED: HRESULT = HRESULT(0x887A0005_u32 as i32);
    const GUID_NULL: GUID = GUID::from_u128(0);

    pub struct MfScreenEncoder {
        tx: mpsc::Sender<EncoderCommand>,
        worker: Option<JoinHandle<()>>,
        accounting: Arc<Accounting>,
        cached: Mutex<Option<CachedOpen>>,
    }

    struct CachedOpen {
        width: u32,
        height: u32,
    }

    #[derive(Default)]
    struct Accounting {
        frames_consumed: AtomicU64,
        samples_written: AtomicU64,
        last_error: Mutex<Option<String>>,
    }

    impl Accounting {
        fn clear_error(&self) {
            if let Ok(mut last_error) = self.last_error.lock() {
                *last_error = None;
            }
        }

        fn set_error(&self, detail: impl Into<String>) {
            if let Ok(mut last_error) = self.last_error.lock() {
                *last_error = Some(detail.into());
            }
        }

        fn last_error(&self) -> Option<String> {
            self.last_error.lock().ok().and_then(|error| error.clone())
        }

        fn health(&self) -> EncoderHealth {
            EncoderHealth {
                frames_consumed: self.frames_consumed.load(Ordering::Relaxed),
                samples_written: self.samples_written.load(Ordering::Relaxed),
                last_error: self.last_error(),
            }
        }
    }

    enum EncoderCommand {
        Open {
            dir: String,
            config: EncoderConfig,
            ack: mpsc::Sender<Result<(), EncoderError>>,
        },
        EncodeFrame(ScreenFrame),
        Finalize {
            ack: mpsc::Sender<Result<(), EncoderError>>,
        },
        Shutdown,
    }

    struct SegmentState {
        dir: PathBuf,
        config: EncoderConfig,
        writer: Option<ActiveWriter>,
        frame_index: u64,
        failure: Option<EncoderError>,
    }

    struct ActiveWriter {
        writer: IMFSinkWriter,
        stream_index: u32,
        partial_path: PathBuf,
        final_path: PathBuf,
    }

    struct MfRuntime;

    impl MfScreenEncoder {
        pub fn new() -> Self {
            let accounting = Arc::new(Accounting::default());
            let (tx, rx) = mpsc::channel();
            let worker_accounting = Arc::clone(&accounting);
            let worker = thread::spawn(move || worker_loop(rx, worker_accounting));
            Self {
                tx,
                worker: Some(worker),
                accounting,
                cached: Mutex::new(None),
            }
        }

        fn worker_stopped_error() -> EncoderError {
            EncoderError::new(EncoderErrorKind::WorkerStopped, "encoder worker stopped")
        }

        fn send_rpc(
            &self,
            make: impl FnOnce(mpsc::Sender<Result<(), EncoderError>>) -> EncoderCommand,
        ) -> Result<(), EncoderError> {
            let (ack_tx, ack_rx) = mpsc::channel();
            self.tx
                .send(make(ack_tx))
                .map_err(|_| Self::worker_stopped_error())?;
            ack_rx.recv().map_err(|_| Self::worker_stopped_error())?
        }
    }

    impl ScreenEncoder for MfScreenEncoder {
        fn open(&mut self, dir: &str, width: u32, height: u32) -> Result<(), EncoderError> {
            let config = EncoderConfig::for_frame_size(width, height);
            self.send_rpc(|ack| EncoderCommand::Open {
                dir: dir.to_string(),
                config,
                ack,
            })?;
            if let Ok(mut cached) = self.cached.lock() {
                *cached = Some(CachedOpen { width, height });
            }
            self.accounting.clear_error();
            Ok(())
        }

        fn encode_frame(&mut self, frame: &ScreenFrame) -> Result<(), EncoderError> {
            self.accounting
                .frames_consumed
                .fetch_add(1, Ordering::Relaxed);

            let dims = self
                .cached
                .lock()
                .ok()
                .and_then(|cached| cached.as_ref().map(|cached| (cached.width, cached.height)));
            let Some((width, height)) = dims else {
                let error = EncoderError::new(EncoderErrorKind::EncodeFailed, "encoder not open");
                self.accounting.set_error(error.detail.clone());
                return Err(error);
            };
            if frame.width != width || frame.height != height {
                let error = EncoderError::new(
                    EncoderErrorKind::InvalidFrameDimensions,
                    format!(
                        "frame dimensions {}x{} do not match opened {}x{}",
                        frame.width, frame.height, width, height
                    ),
                );
                self.accounting.set_error(error.detail.clone());
                return Err(error);
            }

            self.tx
                .send(EncoderCommand::EncodeFrame(frame.clone()))
                .map_err(|_| {
                    let error = Self::worker_stopped_error();
                    self.accounting.set_error(error.detail.clone());
                    error
                })
        }

        fn finalize(&mut self) -> Result<(), EncoderError> {
            let result = self.send_rpc(|ack| EncoderCommand::Finalize { ack });
            if let Ok(mut cached) = self.cached.lock() {
                *cached = None;
            }
            result
        }

        fn frames_consumed(&self) -> u64 {
            self.accounting.frames_consumed.load(Ordering::Relaxed)
        }

        fn samples_written(&self) -> u64 {
            self.accounting.samples_written.load(Ordering::Relaxed)
        }

        fn last_error(&self) -> Option<String> {
            self.accounting.last_error()
        }

        fn health(&self) -> EncoderHealth {
            self.accounting.health()
        }
    }

    impl Drop for MfScreenEncoder {
        fn drop(&mut self) {
            let _ = self.tx.send(EncoderCommand::Shutdown);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn worker_loop(rx: mpsc::Receiver<EncoderCommand>, accounting: Arc<Accounting>) {
        let runtime = match MfRuntime::new() {
            Ok(runtime) => Some(runtime),
            Err(error) => {
                accounting.set_error(error.detail.clone());
                None
            }
        };
        let mut segment: Option<SegmentState> = None;

        while let Ok(command) = rx.recv() {
            match command {
                EncoderCommand::Open { dir, config, ack } => {
                    let result = if runtime.is_some() {
                        segment = Some(SegmentState {
                            dir: PathBuf::from(dir),
                            config,
                            writer: None,
                            frame_index: 0,
                            failure: None,
                        });
                        accounting.clear_error();
                        Ok(())
                    } else {
                        Err(EncoderError::new(
                            EncoderErrorKind::Unavailable,
                            "Media Foundation initialization failed",
                        ))
                    };
                    let _ = ack.send(result);
                }
                EncoderCommand::EncodeFrame(frame) => {
                    if runtime.is_some() {
                        encode_on_worker(&mut segment, frame, &accounting);
                    }
                }
                EncoderCommand::Finalize { ack } => {
                    let result = if runtime.is_some() {
                        finalize_on_worker(&mut segment)
                    } else {
                        Err(EncoderError::new(
                            EncoderErrorKind::Unavailable,
                            "Media Foundation initialization failed",
                        ))
                    };
                    if let Err(error) = &result {
                        accounting.set_error(error.detail.clone());
                    }
                    let _ = ack.send(result);
                }
                EncoderCommand::Shutdown => {
                    if runtime.is_some() {
                        let _ = finalize_on_worker(&mut segment);
                    }
                    break;
                }
            }
        }

        drop(runtime);
    }

    impl MfRuntime {
        fn new() -> Result<Self, EncoderError> {
            // MS Learn: CoInitializeEx + MFStartup/MFShutdown lifecycle.
            // https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mfstartup
            // https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mfshutdown
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .map_err(|error| {
                        windows_error(EncoderErrorKind::Unavailable, "CoInitializeEx", error)
                    })?;
                if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                    CoUninitialize();
                    return Err(windows_error(
                        EncoderErrorKind::Unavailable,
                        "MFStartup",
                        error,
                    ));
                }
            }
            Ok(Self)
        }
    }

    impl Drop for MfRuntime {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }

    fn encode_on_worker(
        segment: &mut Option<SegmentState>,
        frame: ScreenFrame,
        accounting: &Accounting,
    ) {
        let Some(segment) = segment.as_mut() else {
            let error = EncoderError::new(EncoderErrorKind::EncodeFailed, "encoder not open");
            accounting.set_error(error.detail);
            return;
        };
        if segment.failure.is_some() {
            return;
        }
        if segment.writer.is_none() {
            match build_writer(segment) {
                Ok(writer) => segment.writer = Some(writer),
                Err(error) => {
                    accounting.set_error(error.detail.clone());
                    segment.failure = Some(error);
                    return;
                }
            }
        }

        let result = write_frame(segment, &frame);
        match result {
            Ok(()) => {
                accounting.samples_written.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                accounting.set_error(error.detail.clone());
                segment.failure = Some(error);
            }
        }
    }

    fn finalize_on_worker(segment: &mut Option<SegmentState>) -> Result<(), EncoderError> {
        let Some(mut segment) = segment.take() else {
            return Ok(());
        };
        if let Some(error) = segment.failure.take() {
            return Err(error);
        }
        let Some(active) = segment.writer.take() else {
            return Ok(());
        };

        let ActiveWriter {
            writer,
            partial_path,
            final_path,
            ..
        } = active;
        unsafe {
            writer.Finalize().map_err(|error| {
                windows_error(
                    classify_hresult(error.code(), EncoderErrorKind::FinalizeFailed),
                    "IMFSinkWriter::Finalize",
                    error,
                )
            })?;
        }
        drop(writer);
        std::fs::rename(&partial_path, &final_path).map_err(|error| {
            EncoderError::new(
                EncoderErrorKind::FinalizeFailed,
                format!(
                    "rename {} -> {} failed: {error}",
                    partial_path.display(),
                    final_path.display()
                ),
            )
        })
    }

    fn build_writer(segment: &SegmentState) -> Result<ActiveWriter, EncoderError> {
        let final_path = segment.dir.join(SCREEN_FILE_NAME);
        let partial_path = segment
            .dir
            .join(format!("{SCREEN_FILE_NAME}{PARTIAL_SUFFIX}"));
        let partial_wide = wide_z(&partial_path);

        // MS Learn sink-writer path:
        // https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-mfcreatesinkwriterfromurl
        // https://learn.microsoft.com/en-us/windows/win32/medfound/tutorial--using-the-sink-writer-to-encode-video
        // CPU samples leave MF_SINK_WRITER_D3D_MANAGER unset; hardware MFTs are enabled without use-only fallback.
        let attrs = create_attributes(3)?;
        unsafe {
            // The sink writer infers the container from the URL extension UNLESS
            // MF_TRANSCODE_CONTAINERTYPE is set. Our output URL ends in `.partial`
            // (the seal-only-after-Finalize marker), which MF does not recognize —
            // without this attribute MFCreateSinkWriterFromURL returns
            // MF_E_NOT_FOUND (0xC00D36D5). Pin MPEG-4 explicitly so the `.partial`
            // name stays decoupled from container selection. (MS Learn:
            // mfreadwrite/nf-mfreadwrite-mfcreatesinkwriterfromurl — Remarks.)
            attrs
                .SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set container type", error)
                })?;
            attrs
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                .map_err(|error| {
                    windows_error(
                        EncoderErrorKind::OpenFailed,
                        "set hardware transforms",
                        error,
                    )
                })?;
            attrs
                .SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)
                .map_err(|error| {
                    windows_error(
                        EncoderErrorKind::OpenFailed,
                        "set disable throttling",
                        error,
                    )
                })?;
        }

        let writer = unsafe {
            MFCreateSinkWriterFromURL(PCWSTR(partial_wide.as_ptr()), None, Some(&attrs)).map_err(
                |error| {
                    windows_error(
                        EncoderErrorKind::OpenFailed,
                        "MFCreateSinkWriterFromURL",
                        error,
                    )
                },
            )?
        };

        let output_type = output_media_type(&segment.config)?;
        let stream_index = unsafe {
            writer.AddStream(&output_type).map_err(|error| {
                windows_error(
                    EncoderErrorKind::OpenFailed,
                    "IMFSinkWriter::AddStream",
                    error,
                )
            })?
        };
        let input_type = input_media_type(&segment.config)?;
        unsafe {
            writer
                .SetInputMediaType(stream_index, &input_type, None)
                .map_err(|error| {
                    windows_error(
                        EncoderErrorKind::OpenFailed,
                        "IMFSinkWriter::SetInputMediaType",
                        error,
                    )
                })?;
        }

        // MS Learn: CODECAPI_AVEncMPVGOPSize through GetServiceForStream/ICodecAPI.
        // Some MFTs reject GOP settings; this is best-effort and must not fail the stream.
        let _ = set_gop_size_best_effort(&writer, stream_index, segment.config.gop_size);

        unsafe {
            writer.BeginWriting().map_err(|error| {
                windows_error(
                    EncoderErrorKind::OpenFailed,
                    "IMFSinkWriter::BeginWriting",
                    error,
                )
            })?;
        }

        Ok(ActiveWriter {
            writer,
            stream_index,
            partial_path,
            final_path,
        })
    }

    fn output_media_type(config: &EncoderConfig) -> Result<IMFMediaType, EncoderError> {
        let media_type = create_media_type()?;
        unsafe {
            media_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set output major type", error)
                })?;
            media_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set output subtype", error)
                })?;
            media_type
                .SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set output bitrate", error)
                })?;
            media_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set output interlace", error)
                })?;
            media_type
                .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set H.264 profile", error)
                })?;
            set_size(&media_type, &MF_MT_FRAME_SIZE, config.width, config.height)?;
            set_ratio(
                &media_type,
                &MF_MT_FRAME_RATE,
                config.frame_rate_num,
                config.frame_rate_den,
            )?;
            set_ratio(
                &media_type,
                &MF_MT_PIXEL_ASPECT_RATIO,
                config.pixel_aspect_num,
                config.pixel_aspect_den,
            )?;
        }
        Ok(media_type)
    }

    fn input_media_type(config: &EncoderConfig) -> Result<IMFMediaType, EncoderError> {
        let media_type = create_media_type()?;
        unsafe {
            media_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set input major type", error)
                })?;
            media_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set input subtype", error)
                })?;
            media_type
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(|error| {
                    windows_error(EncoderErrorKind::OpenFailed, "set input interlace", error)
                })?;
            set_size(&media_type, &MF_MT_FRAME_SIZE, config.width, config.height)?;
            set_ratio(
                &media_type,
                &MF_MT_FRAME_RATE,
                config.frame_rate_num,
                config.frame_rate_den,
            )?;
            set_ratio(
                &media_type,
                &MF_MT_PIXEL_ASPECT_RATIO,
                config.pixel_aspect_num,
                config.pixel_aspect_den,
            )?;
        }
        Ok(media_type)
    }

    fn write_frame(segment: &mut SegmentState, frame: &ScreenFrame) -> Result<(), EncoderError> {
        let active = segment
            .writer
            .as_ref()
            .expect("writer exists before write_frame");
        let nv12 = observer_nv12::rgba_or_bgra_to_nv12(frame).map_err(|error| {
            EncoderError::new(
                EncoderErrorKind::EncodeFailed,
                format!("NV12 conversion failed: {error}"),
            )
        })?;
        let len: u32 = nv12.bytes.len().try_into().map_err(|_| {
            EncoderError::new(
                EncoderErrorKind::EncodeFailed,
                format!("NV12 buffer too large: {} bytes", nv12.bytes.len()),
            )
        })?;

        // MS Learn CPU sample path: MFCreateMemoryBuffer/MFCreateSample/WriteSample.
        // https://learn.microsoft.com/en-us/windows/win32/medfound/tutorial--using-the-sink-writer-to-encode-video
        let buffer = unsafe {
            MFCreateMemoryBuffer(len).map_err(|error| {
                windows_error(
                    EncoderErrorKind::EncodeFailed,
                    "MFCreateMemoryBuffer",
                    error,
                )
            })?
        };
        let mut dst = ptr::null_mut();
        unsafe {
            buffer.Lock(&mut dst, None, None).map_err(|error| {
                windows_error(
                    EncoderErrorKind::EncodeFailed,
                    "IMFMediaBuffer::Lock",
                    error,
                )
            })?;
            ptr::copy_nonoverlapping(nv12.bytes.as_ptr(), dst, nv12.bytes.len());
            buffer.Unlock().map_err(|error| {
                windows_error(
                    EncoderErrorKind::EncodeFailed,
                    "IMFMediaBuffer::Unlock",
                    error,
                )
            })?;
            buffer.SetCurrentLength(len).map_err(|error| {
                windows_error(
                    EncoderErrorKind::EncodeFailed,
                    "IMFMediaBuffer::SetCurrentLength",
                    error,
                )
            })?;
        }

        let sample = unsafe {
            MFCreateSample().map_err(|error| {
                windows_error(EncoderErrorKind::EncodeFailed, "MFCreateSample", error)
            })?
        };
        unsafe {
            sample.AddBuffer(&buffer).map_err(|error| {
                windows_error(
                    EncoderErrorKind::EncodeFailed,
                    "IMFSample::AddBuffer",
                    error,
                )
            })?;
            sample
                .SetSampleTime((segment.frame_index as i64) * FRAME_DURATION_100NS)
                .map_err(|error| {
                    windows_error(
                        EncoderErrorKind::EncodeFailed,
                        "IMFSample::SetSampleTime",
                        error,
                    )
                })?;
            sample
                .SetSampleDuration(FRAME_DURATION_100NS)
                .map_err(|error| {
                    windows_error(
                        EncoderErrorKind::EncodeFailed,
                        "IMFSample::SetSampleDuration",
                        error,
                    )
                })?;
            active
                .writer
                .WriteSample(active.stream_index, &sample)
                .map_err(|error| {
                    windows_error(
                        classify_hresult(error.code(), EncoderErrorKind::EncodeFailed),
                        "IMFSinkWriter::WriteSample",
                        error,
                    )
                })?;
        }
        segment.frame_index = segment.frame_index.saturating_add(1);
        Ok(())
    }

    fn set_gop_size_best_effort(
        writer: &IMFSinkWriter,
        stream_index: u32,
        gop_size: u32,
    ) -> Result<(), EncoderError> {
        let mut raw = ptr::null_mut();
        unsafe {
            writer
                .GetServiceForStream(stream_index, &GUID_NULL, &ICodecAPI::IID, &mut raw)
                .map_err(|error| {
                    windows_error(
                        EncoderErrorKind::OpenFailed,
                        "GetServiceForStream(ICodecAPI)",
                        error,
                    )
                })?;
            let codec = ICodecAPI::from_raw(raw as _);
            let value = VARIANT::from(gop_size);
            codec
                .SetValue(&CODECAPI_AVEncMPVGOPSize, &value)
                .map_err(|error| {
                    windows_error(
                        EncoderErrorKind::OpenFailed,
                        "ICodecAPI::SetValue(GOP)",
                        error,
                    )
                })?;
        }
        Ok(())
    }

    fn create_attributes(initial_size: u32) -> Result<IMFAttributes, EncoderError> {
        let mut attrs = None;
        unsafe {
            MFCreateAttributes(&mut attrs, initial_size).map_err(|error| {
                windows_error(EncoderErrorKind::OpenFailed, "MFCreateAttributes", error)
            })?;
        }
        attrs.ok_or_else(|| {
            EncoderError::new(
                EncoderErrorKind::OpenFailed,
                "MFCreateAttributes returned None",
            )
        })
    }

    fn create_media_type() -> Result<IMFMediaType, EncoderError> {
        unsafe {
            MFCreateMediaType().map_err(|error| {
                windows_error(EncoderErrorKind::OpenFailed, "MFCreateMediaType", error)
            })
        }
    }

    fn set_size(
        media_type: &IMFMediaType,
        key: &windows::core::GUID,
        width: u32,
        height: u32,
    ) -> Result<(), EncoderError> {
        set_u64(media_type, key, ((width as u64) << 32) | height as u64)
    }

    fn set_ratio(
        media_type: &IMFMediaType,
        key: &windows::core::GUID,
        numerator: u32,
        denominator: u32,
    ) -> Result<(), EncoderError> {
        set_u64(
            media_type,
            key,
            ((numerator as u64) << 32) | denominator as u64,
        )
    }

    fn set_u64(
        media_type: &IMFMediaType,
        key: &windows::core::GUID,
        value: u64,
    ) -> Result<(), EncoderError> {
        unsafe {
            media_type.SetUINT64(key, value).map_err(|error| {
                windows_error(
                    EncoderErrorKind::OpenFailed,
                    "set UINT64 media attribute",
                    error,
                )
            })
        }
    }

    fn wide_z(path: &std::path::Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn classify_hresult(code: HRESULT, fallback: EncoderErrorKind) -> EncoderErrorKind {
        if is_device_lost(code) {
            EncoderErrorKind::DeviceLost
        } else {
            fallback
        }
    }

    fn is_device_lost(code: HRESULT) -> bool {
        matches!(
            code,
            DXGI_ERROR_DEVICE_REMOVED
                | MF_E_DXGI_DEVICE_NOT_INITIALIZED
                | MF_E_DXGI_NEW_VIDEO_DEVICE
                | MF_E_HW_MFT_FAILED_START_STREAMING
                | MF_E_NEW_VIDEO_DEVICE
        )
    }

    fn windows_error(
        kind: EncoderErrorKind,
        context: &str,
        error: windows::core::Error,
    ) -> EncoderError {
        EncoderError::new(
            kind,
            format!("{context} failed: {:?}: {error}", error.code()),
        )
    }
}

#[cfg(not(windows))]
mod imp {
    use observer_model::{EncoderError, EncoderHealth, ScreenEncoder, ScreenFrame};

    /// Inert non-Windows stub: records no file and produces no samples.
    #[derive(Debug, Default)]
    pub struct MfScreenEncoder;

    impl MfScreenEncoder {
        pub fn new() -> Self {
            Self
        }
    }

    impl ScreenEncoder for MfScreenEncoder {
        fn open(&mut self, _dir: &str, _width: u32, _height: u32) -> Result<(), EncoderError> {
            Ok(())
        }

        fn encode_frame(&mut self, _frame: &ScreenFrame) -> Result<(), EncoderError> {
            Ok(())
        }

        fn finalize(&mut self) -> Result<(), EncoderError> {
            Ok(())
        }

        fn frames_consumed(&self) -> u64 {
            0
        }

        fn samples_written(&self) -> u64 {
            0
        }

        fn last_error(&self) -> Option<String> {
            None
        }

        fn health(&self) -> EncoderHealth {
            EncoderHealth::default()
        }
    }
}

pub use imp::MfScreenEncoder;
