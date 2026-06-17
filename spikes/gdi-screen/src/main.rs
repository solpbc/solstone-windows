// capture-spike v1: prove screen capture works headlessly on the autologin
// session (out-of-band display) via GDI BitBlt -> PNG. Next: WGC + WASAPI.
use std::ffi::c_void;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

fn main() {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        println!("virtual desktop: {w}x{h} at ({x},{y})");
        if w == 0 || h == 0 {
            eprintln!("ERROR: zero-size desktop (no display?) — capture would be blank");
            std::process::exit(2);
        }

        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY).expect("BitBlt failed");

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // negative = top-down rows
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0 as u32;

        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        println!("GetDIBits scanlines: {lines}");

        // BGRA (GDI) -> RGBA (image), force opaque alpha
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }

        // crude non-blank check: are there >1 distinct pixel values?
        let first = &buf[0..3];
        let varied = buf.chunks_exact(4).any(|p| &p[0..3] != first);
        println!("non-blank (varied pixels): {varied}");

        let img = image::RgbaImage::from_raw(w as u32, h as u32, buf).expect("from_raw");
        std::fs::create_dir_all("C:\\Temp").ok();
        img.save("C:\\Temp\\screenshot.png").expect("save png");
        println!("SAVED C:\\Temp\\screenshot.png ({w}x{h})");

        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}
