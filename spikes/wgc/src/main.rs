use std::io::{self, Write};
use std::time::{Duration, Instant};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::ImageFormat;
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

struct Probe {
    start: Instant,
    frames: u64,
    saved: bool,
    seconds: u64,
    out: String,
}

impl GraphicsCaptureApiHandler for Probe {
    type Flags = (u64, String);
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            start: Instant::now(),
            frames: 0,
            saved: false,
            seconds: ctx.flags.0,
            out: ctx.flags.1,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        self.frames += 1;
        if !self.saved {
            println!("first WGC frame: {}x{}", frame.width(), frame.height());
            frame.save_as_image(&self.out, ImageFormat::Png)?;
            println!("saved first frame: {}", self.out);
            self.saved = true;
        }
        print!(
            "\rWGC running: {}s, frames={}",
            self.start.elapsed().as_secs(),
            self.frames
        );
        io::stdout().flush()?;
        if self.start.elapsed() >= Duration::from_secs(self.seconds) {
            println!("\nWGC_CAPTURE_OK: frames={} elapsed={}s", self.frames, self.seconds);
            capture_control.stop();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        println!("WGC item closed");
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(10);
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "C:\\Temp\\wgc-frame.png".to_string());
    std::fs::create_dir_all("C:\\Temp")?;

    let monitors = Monitor::enumerate()?;
    println!("active monitors: {}", monitors.len());
    for (index, monitor) in monitors.iter().enumerate() {
        println!(
            "monitor #{index}: name={} device={} {}x{} refresh={:?}",
            monitor.name().unwrap_or_default(),
            monitor.device_name().unwrap_or_default(),
            monitor.width().unwrap_or_default(),
            monitor.height().unwrap_or_default(),
            monitor.refresh_rate()
        );
    }

    let monitor = Monitor::primary()?;
    println!(
        "capturing primary monitor {}x{} via Windows.Graphics.Capture",
        monitor.width().unwrap_or_default(),
        monitor.height().unwrap_or_default()
    );
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::Default,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Rgba8,
        (seconds, out),
    );
    Probe::start(settings)?;
    Ok(())
}
