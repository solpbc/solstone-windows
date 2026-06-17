use std::time::{Duration, Instant};
use windows::core::{Error, Result};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

fn main() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoints = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let count = endpoints.GetCount()?;
        println!("active eCapture endpoints: {count}");

        if count == 0 {
            println!("MIC_CAPTURE_UNAVAILABLE: no active WASAPI capture endpoint");
            return Ok(());
        }

        for index in 0..count {
            let device = endpoints.Item(index)?;
            println!("capture endpoint #{index}: {}", device.GetId()?.to_string()?);
        }

        let device = match enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
            Ok(device) => device,
            Err(err) => {
                println!("MIC_CAPTURE_UNAVAILABLE: default eCapture endpoint failed: {err}");
                return Ok(());
            }
        };

        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let pwfx = client.GetMixFormat()?;
        let wfx = &*pwfx;
        let sr = wfx.nSamplesPerSec;
        let ch = wfx.nChannels;
        let bits = wfx.wBitsPerSample;
        let tag = wfx.wFormatTag;
        println!("capture endpoint mix format: {sr} Hz, {ch} ch, {bits} bits, tag {tag}");

        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            0,
            10_000_000,
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
        while start.elapsed() < Duration::from_secs(5) {
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

        client.Stop().map_err(Error::from)?;
        println!("captured {total_frames} frames over ~5s; non-zero bytes: {nonzero_bytes}");
        println!("MIC_CAPTURE_OK: endpoint exists and WASAPI capture loop ran");
        Ok(())
    }
}
