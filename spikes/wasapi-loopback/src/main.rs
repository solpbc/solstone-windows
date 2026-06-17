// capture-spike audio: prove WASAPI loopback (system-audio) capture works in
// the interactive Session 1. Counts frames + non-zero bytes over ~3s.
use std::time::{Duration, Instant};
use windows::core::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

fn main() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let pwfx = client.GetMixFormat()?;
        let wfx = &*pwfx;
        // WAVEFORMATEX is repr(packed): copy fields to locals (no refs to packed fields)
        let sr = wfx.nSamplesPerSec;
        let ch = wfx.nChannels;
        let bits = wfx.wBitsPerSample;
        let tag = wfx.wFormatTag;
        println!("render endpoint mix format: {sr} Hz, {ch} ch, {bits} bits, tag {tag}");

        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            10_000_000, // 1s buffer (100ns units)
            0,
            pwfx,
            None,
        )?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let bpf = (ch as usize) * (bits as usize) / 8;
        let mut total_frames: u64 = 0;
        let mut nonzero_bytes: u64 = 0;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(100));
            loop {
                if capture.GetNextPacketSize()? == 0 {
                    break;
                }
                let mut pdata: *mut u8 = std::ptr::null_mut();
                let mut nframes: u32 = 0;
                let mut flags: u32 = 0;
                capture.GetBuffer(&mut pdata, &mut nframes, &mut flags, None, None)?;
                if nframes > 0 && !pdata.is_null() {
                    let bytes = std::slice::from_raw_parts(pdata, (nframes as usize) * bpf);
                    nonzero_bytes += bytes.iter().filter(|&&b| b != 0).count() as u64;
                    total_frames += nframes as u64;
                }
                capture.ReleaseBuffer(nframes)?;
            }
        }
        client.Stop()?;
        println!("captured {total_frames} frames over ~3s; non-zero bytes: {nonzero_bytes}");
        println!("(pipeline OK either way; non-zero bytes>0 = real audio was playing)");
        println!("AUDIO LOOPBACK OK");
        CoUninitialize();
        Ok(())
    }
}
