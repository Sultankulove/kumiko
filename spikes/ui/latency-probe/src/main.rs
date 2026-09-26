//! Key-to-pixel latency of a prototype running with `SPIKE_BENCH=probe` (Windows).
//!
//! The prototype draws a marker square at `workload::MARKER_X/Y` of its window and flips
//! its colour between black and white on every `x` it receives. The probe presses `x` with
//! `SendInput` and polls that screen pixel until it flips: the time includes input dispatch,
//! the toolkit's update, layout and render, and DWM composition — everything but scan-out.
//!
//! Usage: `latency-probe "<window title>" [samples]`. Prints one JSON line.

#[cfg(not(windows))]
fn main() {
    eprintln!("latency-probe runs on Windows only");
}

#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{HWND, POINT};
    use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, GetDC, GetPixel, ReleaseDC};
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow};

    let mut args = std::env::args().skip(1);
    let title = args
        .next()
        .unwrap_or_else(|| "Kumiko UI spike: iced".into());
    let samples: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let wide: Vec<u16> = title.encode_utf16().chain([0]).collect();

    // SAFETY: plain Win32 calls with valid, null-terminated arguments; handles are checked.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let window: HWND = FindWindowW(std::ptr::null(), wide.as_ptr());
        if window.is_null() {
            eprintln!("window {title:?} not found");
            std::process::exit(2);
        }
        SetForegroundWindow(window);
        std::thread::sleep(Duration::from_millis(500));
        let scale = GetDpiForWindow(window) as f64 / 96.0;
        let centre = |at: f32| f64::from(at + workload::MARKER_SIZE / 2.0) * scale;
        let mut point = POINT {
            x: centre(workload::MARKER_X) as i32,
            y: centre(workload::MARKER_Y) as i32,
        };
        ClientToScreen(window, &mut point);
        let screen = GetDC(std::ptr::null_mut());

        let key = |up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: u16::from(b'X'),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let mut latencies = Vec::with_capacity(samples);
        let mut timeouts = 0;
        let mut poll_cost = Vec::new();
        for _ in 0..samples {
            let before = GetPixel(screen, point.x, point.y);
            let inputs = [key(false), key(true)];
            let sent = Instant::now();
            SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32);
            loop {
                let poll = Instant::now();
                let now = GetPixel(screen, point.x, point.y);
                poll_cost.push(poll.elapsed());
                if now != before {
                    latencies.push(sent.elapsed());
                    break;
                }
                if sent.elapsed() > Duration::from_secs(1) {
                    timeouts += 1;
                    break;
                }
            }
            // Let the prototype settle and vary the phase against vsync.
            std::thread::sleep(Duration::from_millis(
                80 + (latencies.len() as u64 * 7) % 40,
            ));
        }
        ReleaseDC(std::ptr::null_mut(), screen);

        let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
        let mut sorted: Vec<f64> = latencies.iter().map(ms).collect();
        sorted.sort_by(f64::total_cmp);
        let pct = |p: f64| {
            if sorted.is_empty() {
                f64::NAN
            } else {
                sorted[((sorted.len() - 1) as f64 * p).round() as usize]
            }
        };
        let mean_poll = poll_cost.iter().map(ms).sum::<f64>() / poll_cost.len().max(1) as f64;
        println!(
            "{{\"window\":{title:?},\"label\":{:?},\"n\":{},\"timeouts\":{timeouts},\"p50\":{:.2},\"p95\":{:.2},\"max\":{:.2},\"poll_ms\":{mean_poll:.2}}}",
            std::env::var("SPIKE_LABEL").unwrap_or_default(),
            sorted.len(),
            pct(0.5),
            pct(0.95),
            pct(1.0),
        );
    }
}
