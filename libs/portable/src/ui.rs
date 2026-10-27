//! The splash the launcher shows while RTDESK starts: a card with the RTDesk logo that animates in,
//! a loading bar, a shimmer across the logo while it waits, and a fade out once the app's window is
//! up (`close`). Drawn per pixel into a layered window, so it has a soft shadow, rounded corners and
//! stays sharp at any display scale; it follows the Windows light/dark setting. The glyphs come
//! pre-rendered from res/splash.bin (splash.py), the rest is computed here.

use std::{
    mem, ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    time::{Duration, Instant},
};
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE, LPARAM, LRESULT, UINT, WPARAM},
        windef::{HWND, POINT, RECT, SIZE},
    },
    um::{
        libloaderapi::{GetModuleHandleW, GetProcAddress, LoadLibraryW},
        timeapi::{timeBeginPeriod, timeEndPeriod},
        wingdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDeviceCaps,
            SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            BLENDFUNCTION, DIB_RGB_COLORS, LOGPIXELSX,
        },
        winreg::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
        winuser::*,
    },
};

const SPLASH: &[u8] = include_bytes!("./res/splash.bin");

// Layout at 100 %, in logical pixels.
const CARD_W: f32 = 340.0;
const CARD_H: f32 = 188.0;
const CARD_RADIUS: f32 = 20.0;
const MARGIN: f32 = 40.0; // room for the shadow around the card
const LOGO_TOP: f32 = 46.0;
const BAR_TOP: f32 = 142.0;
const BAR_W: f32 = 132.0;
const BAR_H: f32 = 4.0;

// Timeline, in milliseconds from the first frame.
const MIN_SHOWN_MS: f32 = 1000.0; // the logo has finished arriving by then
const FADE_OUT_MS: f32 = 260.0;
const GIVE_UP_MS: f32 = 30_000.0; // never left on screen if nobody closes it

static CLOSE: AtomicBool = AtomicBool::new(false);
static DONE: Mutex<Option<mpsc::Receiver<()>>> = Mutex::new(None);

/// Shows the splash on its own thread.
pub fn setup() {
    let (tx, rx) = mpsc::channel();
    *DONE.lock().unwrap() = Some(rx);
    std::thread::spawn(move || {
        if let Err(e) = unsafe { run() } {
            eprintln!("splash: {}", e);
        }
        let _ = tx.send(());
    });
}

/// Fades the splash out and waits (briefly) until it is gone.
pub fn close() {
    CLOSE.store(true, Ordering::SeqCst);
    if let Some(rx) = DONE.lock().unwrap().take() {
        let _ = rx.recv_timeout(Duration::from_millis((MIN_SHOWN_MS + FADE_OUT_MS) as u64 + 500));
    }
}

type Rgb = [f32; 3];

const fn rgb(hex: u32) -> Rgb {
    [
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ]
}

/// The client's palette (MyTheme and RtdeskLogo in flutter/lib/common.dart).
struct Theme {
    card: Rgb,
    border: Rgb,
    glow: f32,
    shadow: f32,
    ink: Rgb,
    slate: Rgb,
    tagline: Rgb,
    track: Rgb,
    accent: Rgb,
    accent_light: Rgb,
    shimmer: Rgb,
}

const LIGHT: Theme = Theme {
    card: rgb(0xFFFFFF),
    border: rgb(0xE0E1E5),
    glow: 0.05,
    shadow: 0.22,
    ink: rgb(0x141A2B),
    slate: rgb(0x586478),
    tagline: rgb(0x808693),
    track: rgb(0xECEDF3),
    accent: rgb(0x545BE9),
    accent_light: rgb(0x8C9CFB),
    shimmer: rgb(0x545BE9),
};

const DARK: Theme = Theme {
    card: rgb(0x1B1D25),
    border: rgb(0x2C2F3A),
    glow: 0.09,
    shadow: 0.55,
    ink: rgb(0xF4F5F8),
    slate: rgb(0xA1ABBE),
    tagline: rgb(0x808691),
    track: rgb(0x2C2F3A),
    accent: rgb(0x545BE9),
    accent_light: rgb(0x8C9CFB),
    shimmer: rgb(0x8C9CFB),
};

struct Mask {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    data: &'static [u8],
}

/// The masks ("RT", "Desk", tagline) of the scale closest to `scale`, and their group's size.
fn glyphs(scale: f32) -> Option<(i32, i32, Vec<Mask>)> {
    let d = SPLASH;
    if d.len() < 5 || &d[..4] != b"RTSP" {
        return None;
    }
    let u16_at = |p: usize| -> Option<u16> { Some(u16::from_le_bytes([*d.get(p)?, *d.get(p + 1)?])) };
    let want = (scale * 100.0).round() as i32;
    let mut best: Option<(i32, (i32, i32, Vec<Mask>))> = None;
    let mut p = 5;
    for _ in 0..d[4] {
        let set_scale = u16_at(p)? as i32;
        let (w, h, n) = (u16_at(p + 2)? as i32, u16_at(p + 4)? as i32, *d.get(p + 6)?);
        p += 7;
        let mut masks = Vec::new();
        for _ in 0..n {
            let x = u16_at(p)? as i16 as i32;
            let y = u16_at(p + 2)? as i16 as i32;
            let (mw, mh) = (u16_at(p + 4)? as i32, u16_at(p + 6)? as i32);
            p += 8;
            let len = (mw * mh) as usize;
            masks.push(Mask { x, y, w: mw, h: mh, data: d.get(p..p + len)? });
            p += len;
        }
        let diff = (set_scale - want).abs();
        if best.as_ref().map_or(true, |(b, _)| diff < *b) {
            best = Some((diff, (w, h, masks)));
        }
    }
    best.map(|(_, g)| g)
}

fn clamp01(v: f32) -> f32 {
    v.max(0.0).min(1.0)
}

fn ease_out_cubic(t: f32) -> f32 {
    let t = clamp01(t);
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_out_cubic(t: f32) -> f32 {
    let t = clamp01(t);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn lerp(a: Rgb, b: Rgb, k: f32) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * k,
        a[1] + (b[1] - a[1]) * k,
        a[2] + (b[2] - a[2]) * k,
    ]
}

/// Signed distance from (x, y), relative to the centre, to a rounded rectangle.
fn rounded_rect(x: f32, y: f32, half_w: f32, half_h: f32, r: f32) -> f32 {
    let qx = x.abs() - half_w + r;
    let qy = y.abs() - half_h + r;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// `color` at `alpha` over a premultiplied BGRA pixel.
#[inline]
fn over(dst: u32, color: Rgb, alpha: f32) -> u32 {
    if alpha <= 0.0 {
        return dst;
    }
    let keep = 1.0 - alpha;
    let ch = |shift: u32, c: f32| -> u32 {
        let d = ((dst >> shift) & 0xff) as f32;
        (c * 255.0 * alpha + d * keep).round().min(255.0) as u32
    };
    (ch(24, 1.0) << 24) | (ch(16, color[0]) << 16) | (ch(8, color[1]) << 8) | ch(0, color[2])
}

struct Splash {
    w: i32,
    h: i32,
    s: f32,
    theme: &'static Theme,
    base: Vec<u32>,
    group: (i32, i32, i32, i32),
    masks: Vec<Mask>,
    bar: (f32, f32, f32, f32),
}

impl Splash {
    fn new(s: f32, theme: &'static Theme) -> Option<Self> {
        let (gw, gh, masks) = glyphs(s)?;
        let w = ((CARD_W + 2.0 * MARGIN) * s).round() as i32;
        let h = ((CARD_H + 2.0 * MARGIN) * s).round() as i32;
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let (half_w, half_h, r) = (CARD_W * s / 2.0, CARD_H * s / 2.0, CARD_RADIUS * s);
        let card_top = cy - half_h;
        let border = s.round().max(1.0);

        // Card, border, a faint accent glow from the top, and two shadows (soft and contact).
        let mut base = vec![0u32; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = rounded_rect(px, py, half_w, half_h, r);
                let soft = rounded_rect(px, py - 10.0 * s, half_w, half_h, r).max(0.0) / (18.0 * s);
                let tight = rounded_rect(px, py - 1.5 * s, half_w, half_h, r).max(0.0) / (3.0 * s);
                let shadow = theme.shadow * (-soft * soft).exp() + 0.08 * (-tight * tight).exp();
                let mut pixel = over(0, [0.0; 3], clamp01(shadow));

                let card = clamp01(0.5 - d);
                if card > 0.0 {
                    let gx = px / (half_w * 0.9);
                    let gy = (y as f32 + 0.5 - card_top) / (CARD_H * s * 0.6);
                    let glow = theme.glow * (-(gx * gx + gy * gy)).exp();
                    let edge = (card - clamp01(0.5 - (d + border))) / card;
                    let color = lerp(lerp(theme.card, theme.accent, glow), theme.border, edge);
                    pixel = over(pixel, color, card);
                }
                base[(y * w + x) as usize] = pixel;
            }
        }
        let group = (
            ((w - gw) as f32 / 2.0).round() as i32,
            (card_top + LOGO_TOP * s).round() as i32,
            gw,
            gh,
        );
        let (bar_w, bar_h) = (BAR_W * s, (BAR_H * s).round().max(3.0));
        let bar = (cx - bar_w / 2.0, (card_top + BAR_TOP * s).round(), bar_w, bar_h);
        Some(Self { w, h, s, theme, base, group, masks, bar })
    }

    fn draw(&self, t: f32, out: &mut [u32]) {
        out.copy_from_slice(&self.base);
        let th = self.theme;
        let (gx, gy, gw, _) = self.group;

        // A light sweep across the logo every few seconds once it has arrived.
        let sweep = if t > 1300.0 {
            let c = (t - 1300.0) % 2800.0;
            (c < 1100.0).then(|| gx as f32 + (-0.25 + 1.5 * ease_in_out_cubic(c / 1100.0)) * gw as f32)
        } else {
            None
        };
        let band = 22.0 * self.s;

        // "RT" from the left, "Desk" from the right, then the tagline rising.
        let parts = [
            (th.ink, 120.0, -14.0, 0.0),
            (th.slate, 240.0, 14.0, 0.0),
            (th.tagline, 470.0, 0.0, 7.0),
        ];
        for (mask, &(color, start, from_x, from_y)) in self.masks.iter().zip(parts.iter()) {
            let k = ease_out_cubic((t - start) / 560.0);
            if k <= 0.0 {
                continue;
            }
            let ox = gx + mask.x + (from_x * self.s * (1.0 - k)).round() as i32;
            let oy = gy + mask.y + (from_y * self.s * (1.0 - k)).round() as i32;
            for my in 0..mask.h {
                let y = oy + my;
                if y < 0 || y >= self.h {
                    continue;
                }
                for mx in 0..mask.w {
                    let x = ox + mx;
                    let coverage = mask.data[(my * mask.w + mx) as usize];
                    if coverage == 0 || x < 0 || x >= self.w {
                        continue;
                    }
                    let mut c = color;
                    if let Some(center) = sweep {
                        let dist = (x as f32 + 0.35 * (y - gy) as f32 - center) / band;
                        c = lerp(c, th.shimmer, 0.9 * (-dist * dist).exp());
                    }
                    let i = (y * self.w + x) as usize;
                    out[i] = over(out[i], c, coverage as f32 / 255.0 * k);
                }
            }
        }

        // The loading bar: a track, and a segment that grows from the left and leaves on the right.
        let shown = ease_out_cubic((t - 560.0) / 420.0);
        if shown <= 0.0 {
            return;
        }
        let (bx, by, bw, bh) = self.bar;
        let phase = ((t - 560.0).max(0.0) % 1700.0) / 1700.0;
        let head = bx + bw * ease_in_out_cubic(phase / 0.72);
        let tail = bx + bw * ease_in_out_cubic((phase - 0.28) / 0.72);
        let radius = bh / 2.0;
        let (x0, x1) = (bx.floor() as i32 - 1, (bx + bw).ceil() as i32 + 1);
        let (y0, y1) = (by.floor() as i32 - 1, (by + bh).ceil() as i32 + 1);
        for y in y0.max(0)..y1.min(self.h) {
            for x in x0.max(0)..x1.min(self.w) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let track = rounded_rect(px - (bx + bw / 2.0), py - (by + bh / 2.0), bw / 2.0, bh / 2.0, radius);
                let i = (y * self.w + x) as usize;
                out[i] = over(out[i], th.track, clamp01(0.5 - track) * shown);
                if head - tail > 0.5 {
                    let seg = rounded_rect(
                        px - (tail + head) / 2.0,
                        py - (by + bh / 2.0),
                        (head - tail) / 2.0,
                        bh / 2.0,
                        radius.min((head - tail) / 2.0),
                    )
                    .max(track);
                    let along = clamp01((px - tail) / (head - tail).max(1.0));
                    let color = lerp(th.accent, th.accent_light, along);
                    out[i] = over(out[i], color, clamp01(0.5 - seg) * shown);
                }
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Windows' "app mode" setting: dark unless apps are set to light. RTDESK_SPLASH_THEME=dark|light
/// overrides it, for previewing both.
unsafe fn dark_mode() -> bool {
    match std::env::var("RTDESK_SPLASH_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let name = wide("AppsUseLightTheme");
    let mut value: DWORD = 1;
    let mut size = mem::size_of::<DWORD>() as DWORD;
    let status = RegGetValueW(
        HKEY_CURRENT_USER,
        key.as_ptr(),
        name.as_ptr(),
        RRF_RT_REG_DWORD,
        ptr::null_mut(),
        &mut value as *mut DWORD as *mut _,
        &mut size,
    );
    status == 0 && value == 0
}

/// The work area of the monitor under the cursor, and its scale (Shcore's per-monitor DPI where
/// there is one, the system DPI on Windows 7).
unsafe fn monitor() -> (RECT, f32) {
    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    let hmon = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = mem::zeroed();
    info.cbSize = mem::size_of::<MONITORINFO>() as DWORD;
    GetMonitorInfoW(hmon, &mut info);

    let mut dpi = 0u32;
    let shcore = LoadLibraryW(wide("shcore.dll").as_ptr());
    if !shcore.is_null() {
        let f = GetProcAddress(shcore, b"GetDpiForMonitor\0".as_ptr() as _);
        if !f.is_null() {
            let get: unsafe extern "system" fn(_, u32, *mut u32, *mut u32) -> i32 = mem::transmute(f);
            let mut dpi_y = 0u32;
            if get(hmon, 0, &mut dpi, &mut dpi_y) != 0 {
                dpi = 0;
            }
        }
    }
    if dpi == 0 {
        let screen = GetDC(ptr::null_mut());
        dpi = GetDeviceCaps(screen, LOGPIXELSX) as u32;
        ReleaseDC(ptr::null_mut(), screen);
    }
    (info.rcWork, (dpi.max(96) as f32 / 96.0).min(3.0))
}

unsafe fn run() -> Result<(), String> {
    let (work, scale) = monitor();
    let splash = Splash::new(scale, if dark_mode() { &DARK } else { &LIGHT })
        .ok_or("res/splash.bin is unreadable")?;
    let (w, h) = (splash.w, splash.h);
    let x = work.left + ((work.right - work.left) - w) / 2;
    let y = work.top + ((work.bottom - work.top) - h) / 2 - (h as f32 * 0.04) as i32;

    let instance = GetModuleHandleW(ptr::null());
    let class = wide("RtdeskSplash");
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: ptr::null_mut(),
        hCursor: LoadCursorW(ptr::null_mut(), IDC_APPSTARTING),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class.as_ptr(),
    };
    RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        class.as_ptr(),
        wide("RTDESK").as_ptr(),
        WS_POPUP,
        x,
        y,
        w,
        h,
        ptr::null_mut(),
        ptr::null_mut(),
        instance,
        ptr::null_mut(),
    );
    if hwnd.is_null() {
        return Err("cannot create the window".to_owned());
    }

    let screen = GetDC(ptr::null_mut());
    let memory = CreateCompatibleDC(screen);
    let mut bmi: BITMAPINFO = mem::zeroed();
    bmi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as DWORD;
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut winapi::ctypes::c_void = ptr::null_mut();
    let bitmap = CreateDIBSection(memory, &bmi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
    if bitmap.is_null() || bits.is_null() {
        DeleteDC(memory);
        ReleaseDC(ptr::null_mut(), screen);
        DestroyWindow(hwnd);
        return Err("cannot create the frame buffer".to_owned());
    }
    let previous = SelectObject(memory, bitmap as _);
    let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);

    timeBeginPeriod(1);
    let start = Instant::now();
    let mut closing_at: Option<f32> = None;
    let mut frame: u64 = 0;
    let mut msg: MSG = mem::zeroed();
    'frames: loop {
        while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            if msg.message == WM_QUIT {
                break 'frames;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let t = start.elapsed().as_secs_f32() * 1000.0;
        if closing_at.is_none() && (CLOSE.load(Ordering::SeqCst) || t > GIVE_UP_MS) {
            closing_at = Some(t.max(MIN_SHOWN_MS));
        }
        // In: fade and rise; out: fade and drift up a little.
        let mut opacity = ease_out_cubic(t / 320.0);
        let mut lift = 12.0 * scale * (1.0 - ease_out_cubic(t / 460.0));
        if let Some(at) = closing_at {
            let k = (t - at) / FADE_OUT_MS;
            if k >= 1.0 {
                break;
            }
            if k > 0.0 {
                opacity *= 1.0 - k * k;
                lift -= 6.0 * scale * ease_out_cubic(k);
            }
        }

        splash.draw(t, pixels);
        GdiFlush();
        let mut position = POINT { x, y: y + lift.round() as i32 };
        let mut size = SIZE { cx: w, cy: h };
        let mut source = POINT { x: 0, y: 0 };
        let mut blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER,
            BlendFlags: 0,
            SourceConstantAlpha: (opacity * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA,
        };
        UpdateLayeredWindow(hwnd, screen, &mut position, &mut size, memory, &mut source, 0, &mut blend, ULW_ALPHA);
        if frame == 0 {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }

        frame += 1;
        let next = start + Duration::from_micros(frame * 16_667);
        let wait = next.saturating_duration_since(Instant::now()).as_millis() as DWORD;
        MsgWaitForMultipleObjects(0, ptr::null(), FALSE, wait, QS_ALLINPUT);
    }
    timeEndPeriod(1);

    SelectObject(memory, previous);
    DeleteObject(bitmap as _);
    DeleteDC(memory);
    ReleaseDC(ptr::null_mut(), screen);
    DestroyWindow(hwnd);
    Ok(())
}
