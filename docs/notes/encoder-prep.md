# Encoder prep findings

## Unknown 1 - `windows-capture` v2.0.0 frame API

Local source is present: `find ~/.cargo -path '*windows-capture*' -name '*.rs'` and `ls ~/.cargo/registry/src/*/windows-capture-2.0.0/` both found `windows-capture-2.0.0`, including `src/frame.rs`.

`Frame<'a>` public methods in v2.0.0 are:

- `new(...) -> Self` (`~/.cargo/.../windows-capture-2.0.0/src/frame.rs:84`, docs.rs source: https://docs.rs/crate/windows-capture/2.0.0/source/src/frame.rs#L84)
- `width() -> u32`, returning `self.desc.Width` from `D3D11_TEXTURE2D_DESC` (`frame.rs:97`)
- `dirty_regions() -> Result<Vec<DirtyRegion>, windows::core::Error>` (`frame.rs:103`)
- `height() -> u32`, returning `self.desc.Height` (`frame.rs:114`)
- `timestamp() -> Result<TimeSpan, windows::core::Error>` (`frame.rs:121`)
- `color_format() -> ColorFormat` (`frame.rs:127`)
- `as_raw_surface() -> &IDirect3DSurface` (`frame.rs:134`)
- `as_raw_texture() -> &ID3D11Texture2D` (`frame.rs:141`)
- `device() -> &ID3D11Device` (`frame.rs:148`)
- `device_context() -> &ID3D11DeviceContext` (`frame.rs:155`)
- `desc() -> &D3D11_TEXTURE2D_DESC` (`frame.rs:162`)
- `buffer() -> Result<FrameBuffer<'_>, Error>` (`frame.rs:169`)
- `buffer_crop(...) -> Result<FrameBuffer<'_>, Error>` (`frame.rs:223`)
- `buffer_without_title_bar() -> Result<FrameBuffer<'_>, Error>` (`frame.rs:295`)
- `save_as_image(...) -> Result<(), Error>` (`frame.rs:309`)

`FrameBuffer<'a>` public methods are `new`, `width`, `height`, `row_pitch`, `depth_pitch`, `color_format`, `has_padding`, `as_raw_buffer`, `as_nopadding_buffer`, and `save_as_image` (`frame.rs:337-457`). `DirtyRegion` is a public returned type with `x`, `y`, `width`, and `height` fields (`frame.rs:47-58`).

Answer: a GPU escape hatch exists in v2.0.0. `Frame` exposes `as_raw_surface() -> &IDirect3DSurface` and `as_raw_texture() -> &ID3D11Texture2D`, plus `device()`, `device_context()`, and `desc()`. The CPU path also exists: `buffer()` creates a staging texture, copies `self.frame_texture` into it, maps it, and returns `FrameBuffer` (`frame.rs:169-221`). `frame.width()`/`frame.height()` are the native D3D texture dimensions from `D3D11_TEXTURE2D_DESC.Width/Height`.

## Unknown 2 - Media Foundation H.264 to MP4 pattern

Construction:

- Lifecycle: call `CoInitializeEx`, then `MFStartup(MF_VERSION, ...)` before Media Foundation work, and call `MFShutdown` once for each `MFStartup` before exit. MS Learn: https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mfstartup and https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mfshutdown.
- Plain MP4: `MFCreateSinkWriterFromURL` creates a sink writer from a URL or byte stream; with URL and no byte stream it creates the file, and without `MF_TRANSCODE_CONTAINERTYPE` it selects the container from the extension. MS Learn: https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-mfcreatesinkwriterfromurl.
- Fragmented MP4: `MFCreateFMPEG4MediaSink` is the documented fragmented-MP4 media sink constructor; it takes a writable, seekable `IMFByteStream` plus video/audio media types. Wrap that sink with `MFCreateSinkWriterFromMediaSink`. MS Learn: https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nf-mfidl-mfcreatefmpeg4mediasink and https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-mfcreatesinkwriterfrommediasink.
- `MF_MPEG4SINK_MOOV_BEFORE_MDAT` is not the fMP4 selector. It only changes non-fragmented MP4 box order from default `mdat` then `moov` to `moov` before `mdat`, with extra copying/remuxing. MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/mf-mpeg4sink-moov-before-mdat.
- `MF_TRANSCODE_CONTAINERTYPE` also has `MFTranscodeContainerType_FMPEG4` as a sink-writer container value. MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/mf-transcode-containertype.
- Flush/finalize: `IMFSinkWriter::Flush` flushes the encoder and sends `MFSTREAMSINK_MARKER_ENDOFSEGMENT` to the media sink; `Finalize()` completes writing, and without it output may be incomplete/invalid, for example missing required file headers. MS Learn: https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsinkwriter-flush and https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsinkwriter-finalize. MS Learn confirms Windows 8 MPEG-4 source/sink support movie fragments (`moof`) but not `mfra`: https://learn.microsoft.com/en-us/windows/win32/medfound/mpeg-4-file-sink.

Output encoded H.264 media type:

- Create `IMFMediaType`, then set `MF_MT_MAJOR_TYPE=MFMediaType_Video`, `MF_MT_SUBTYPE=MFVideoFormat_H264`, `MF_MT_AVG_BITRATE`, `MF_MT_INTERLACE_MODE=MFVideoInterlace_Progressive`, `MF_MT_FRAME_SIZE`, `MF_MT_FRAME_RATE`, and `MF_MT_PIXEL_ASPECT_RATIO`; then `IMFSinkWriter::AddStream`. MS Learn sink-writer tutorial shows this exact shape for encoded video, with subtype chosen by the caller: https://learn.microsoft.com/en-us/windows/win32/medfound/tutorial--using-the-sink-writer-to-encode-video.
- Set H.264 profile with `MF_MT_MPEG2_PROFILE`; for H.264 the value is from `eAVEncH264VProfile` such as `eAVEncH264VProfile_High`. MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/mf-mt-mpeg2-profile-attribute and https://learn.microsoft.com/en-us/windows/win32/api/codecapi/ne-codecapi-eavench264vprofile.

Input uncompressed media type:

- Use `MF_MT_MAJOR_TYPE=MFMediaType_Video`, `MF_MT_SUBTYPE=MFVideoFormat_NV12`, `MF_MT_INTERLACE_MODE=MFVideoInterlace_Progressive`, and the same `MF_MT_FRAME_SIZE`, `MF_MT_FRAME_RATE`, and `MF_MT_PIXEL_ASPECT_RATIO`; then `IMFSinkWriter::SetInputMediaType`. The sink-writer tutorial shows the uncompressed-input pattern, and the H.264 encoder page lists `MFVideoFormat_NV12` as a supported input subtype. MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/tutorial--using-the-sink-writer-to-encode-video and https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder.

GOP/keyframe interval:

- The property is `CODECAPI_AVEncMPVGOPSize`, data type `UINT32 (VT_UI4)`, and MS Learn says to set it before recording. MS Learn: https://learn.microsoft.com/en-us/windows/win32/codecapi/avencmpvgopsize-property.
- The H.264 encoder exposes `ICodecAPI`, and its page describes `CODECAPI_AVEncMPVGOPSize` as the number of pictures from one GOP header to the next. MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder.
- Concrete sink-writer route to the encoder: after the stream has an encoder, call `IMFSinkWriter::GetServiceForStream(stream_index, GUID_NULL, ICodecAPI::IID, ...)`; MS Learn says this queries the encoder for the stream unless `MF_SINK_WRITER_MEDIASINK` is passed. Then call `ICodecAPI::SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(90u32))` before writing samples. MS Learn: https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsinkwriter-getserviceforstream.
- Adjacent documented routes exist: `SetInputMediaType` accepts `pEncodingParameters: IMFAttributes` to configure the encoder, and `MF_SINK_WRITER_ENCODER_CONFIG` stores an `IPropertyStore` of encoding properties passed at writer creation. MS Learn: https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsinkwriter-setinputmediatype and https://learn.microsoft.com/en-us/windows/win32/medfound/mf-sink-writer-encoder-config. I did not find an MS Learn page saying `CODECAPI_AVEncMPVGOPSize` specifically should be set as a bare stream `IMFAttributes` key.

Sample feeding:

- CPU NV12 path is the standard sample path: `MFCreateMemoryBuffer`, lock/copy/unlock, `SetCurrentLength`, `MFCreateSample`, `IMFSample::AddBuffer`, `SetSampleTime`, `SetSampleDuration`, and `IMFSinkWriter::WriteSample`. Times/durations are 100 ns units in the tutorial (`10 * 1000 * 1000 / FPS`). MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/tutorial--using-the-sink-writer-to-encode-video.

Hardware/software MFT and D3D manager:

- `MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS=TRUE` enables hardware MFTs; by default the sink writer does not use hardware encoders. `MF_READWRITE_USE_ONLY_HARDWARE_TRANSFORMS` is the one that makes the chain fail if no matching hardware MFT exists. Therefore setting enable-hardware without use-only allows the normal software path when no hardware encoder is usable. MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/mf-readwrite-enable-hardware-transforms and https://learn.microsoft.com/en-us/windows/win32/medfound/mf-readwrite-use-only-hardware-transforms.
- `MF_SINK_WRITER_D3D_MANAGER` provides a Direct3D device to video encoders or media sinks loaded by the sink writer. The CPU tutorial uses memory buffers and creates the writer with no D3D manager, so the confirmed CPU-fed path leaves it unset. I did not find MS Learn wording that says CPU samples "must not" set it. MS Learn: https://learn.microsoft.com/en-us/windows/win32/medfound/sink-writer-attributes.
- `MF_SINK_WRITER_DISABLE_THROTTLING` disables the sink writer's default blocking/pacing in `WriteSample`. MS Learn: https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsinkwriter-writesample.

`windows` crate 0.58 wiring:

- Required features for the direct CPU-fed Media Foundation path: `Win32_Media_MediaFoundation` and `Win32_System_Com`. `Win32_Media_MediaFoundation` contains `MFStartup`, `MFShutdown`, `MFCreateMediaType`, `MFCreateMemoryBuffer`, `MFCreateSample`, `MFCreateSinkWriterFromURL`, `MFCreateFMPEG4MediaSink`, `MFCreateSinkWriterFromMediaSink`, `IMFSinkWriter`, `ICodecAPI`, `CODECAPI_AVEncMPVGOPSize`, `MFVideoFormat_H264`, `MFVideoFormat_NV12`, `MF_MT_*`, `MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS`, `MF_SINK_WRITER_D3D_MANAGER`, and `MF_SINK_WRITER_DISABLE_THROTTLING`. docs.rs source: https://docs.rs/crate/windows/0.58.0/source/src/Windows/Win32/Media/MediaFoundation/mod.rs.
- `Win32_System_Com` contains `CoInitializeEx`, `CoUninitialize`, `COINIT_*`, and `IStream` if an `IStream`-backed byte stream is used. docs.rs source: https://docs.rs/crate/windows/0.58.0/source/src/Windows/Win32/System/Com/mod.rs.
- `windows_core::VARIANT` has `From<u32>` for `VT_UI4`, so setting GOP with `VARIANT::from(90u32)` does not require `Win32_System_Variant`. Add `Win32_System_Variant` only if the implementation wants `VT_UI4` constants or `VariantClear` from the Win32 module. docs.rs source: https://docs.rs/crate/windows-core/0.58.0/source/src/variant.rs#L570.
- `Win32_UI_Shell_PropertiesSystem` is needed only if design uses the `MF_SINK_WRITER_ENCODER_CONFIG`/`IPropertyStore` route (`PSCreateMemoryPropertyStore`, `IPropertyStore`). docs.rs source: https://docs.rs/crate/windows/0.58.0/source/src/Windows/Win32/UI/Shell/PropertiesSystem/mod.rs.
- No requested symbol was missing in local `windows-0.58.0`. I did not find a need for `Win32_Media_KernelStreaming` or `Win32_Media_DirectShow` for the listed Media Foundation sink-writer symbols.

## Unknown 3 - legacy screen-file census

`rg -n "screen\.bin" --hidden` hits:

- `docs/design-1A-capture-core.md:109` and `docs/design-1A-capture-core.md:117` - doc/comment. Historical 1A layout and format-risk docs; update for accuracy when the file contract changes.
- `crates/platform-win/src/lib.rs:64` - product code. `source_file_name(SourceKind::Screen)` held the legacy raw screen filename and had to change to the new MP4 name.
- `crates/platform-win/src/lib.rs:499`, `:527`, `:535`, `:566` - test/fixture. These asserted/read/wrote the legacy screen segment file. The scope-listed `:503` and `:506` were not legacy-screen-file hits in this checkout; they asserted `system-audio.pcm` and `mic.pcm`.
- `crates/pl-transport-win/src/sealed.rs:7` - doc/comment. Module comment lists per-source files.
- `crates/pl-transport-win/src/sealed.rs:146`, `:158`, `:159` - test/fixture. Sealed-store scan/read fixture and expected file list.
- `crates/pl-transport-win/examples/live_gate.rs:63` - fixture/example. Fabricates a sealed segment for live gate upload.
- `crates/observer-pl/src/wire.rs:217` - test/fixture. Segment-list response fixture with uploaded file metadata.
- `crates/observer-pl/src/multipart.rs:68`, `:81` - test/fixture. Multipart body fixture.

Coordinator check: `rg -n "screen\.bin" crates/pl-transport-win/src/coordinator.rs` returned no hits. The coordinator is filename-agnostic: it iterates `segment.files`, reads each name, uses `filename: name.clone()`, derives content type via `content_type_for(name)`, uploads all parts, and reconciles by sha (`crates/pl-transport-win/src/coordinator.rs:68-78`, `:86-99`).

Journal-side parser check in this repo: `rg -n "parse_screen_filename|screen_filename|display_<n>_screen|_screen\.mp4|_screen\b" --hidden` found no parser and no assertion of the `display_<n>_screen.mp4` shape. `parse_screen_filename` does not live in this repo based on that search; it appears to be journal-side only.

## Open questions for design

- MS Learn confirms `MFCreateFMPEG4MediaSink` creates fragmented MP4 and that `Flush` sends `MFSTREAMSINK_MARKER_ENDOFSEGMENT`, but I could not confirm from MS Learn that periodic `IMFSinkWriter::Flush` makes the partial file already playable or crash-recoverable at each flush boundary.
- MS Learn does not spell out exactly which MP4 boxes `IMFSinkWriter::Finalize()` writes for fragmented MP4; it only states that finalization completes the output and omitting it may leave required file headers missing.
- MS Learn confirms the CPU-fed path can leave `MF_SINK_WRITER_D3D_MANAGER` unset, but I could not confirm a strict "must not set D3D manager when feeding CPU samples" rule.
