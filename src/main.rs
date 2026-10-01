#![windows_subsystem = "windows"]

mod config;
mod desktops;
mod render;
mod theme;

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromWindow,
    ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{
    IVirtualDesktopManager, Shell_NotifyIconW, VirtualDesktopManager, NIF_ICON, NIF_MESSAGE,
    NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{Placement, Settings};
use desktops::Desktops;
use render::Canvas;
use theme::Theme;

const WM_TRAY: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;

type WndProcFn = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
const TIMER_TICK: usize = 1;
const TICK_MS: u32 = 200;
const TIMER_ANIM: usize = 2;
const ANIM_FRAME_MS: u32 = 15;
const ANIM_DURATION: f32 = 0.22;

const HSHELL_WINDOWDESTROYED: usize = 2;
const HSHELL_WINDOWACTIVATED: usize = 4;
const HSHELL_RUDEAPPACTIVATED: usize = 0x8004;
const HSHELL_FLASH: usize = 0x8006;

const CMD_DOTS: usize = 1;
const CMD_STARTUP: usize = 2;
const CMD_PLACE_AUTO: usize = 10;
const CMD_PLACE_LEFT: usize = 11;
const CMD_PLACE_CENTER: usize = 12;
const CMD_EXIT: usize = 99;

/// Everything that affects what is drawn; redraw only happens when this changes.
#[derive(Clone, PartialEq, Debug)]
struct View {
    visible: bool,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    cell: i32,
    gap: i32,
    pad: i32,
    /// Cells stack top-to-bottom on left/right docked taskbars.
    vertical: bool,
    /// Highlight position in cells; fractional while sliding between desktops.
    highlight: f32,
    scale: f32,
    current: usize,
    dots: Vec<bool>,
    hover: Option<usize>,
    theme: Theme,
}

impl View {
    fn cell_rect(&self, i: usize) -> RECT {
        let off = self.pad + i as i32 * (self.cell + self.gap);
        let (x0, y0) = if self.vertical {
            (self.pad, off)
        } else {
            (off, self.pad)
        };
        RECT {
            left: x0,
            top: y0,
            right: x0 + self.cell,
            bottom: y0 + self.cell,
        }
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.dots.len()).find(|&i| {
            let r = self.cell_rect(i);
            x >= r.left && x < r.right && y >= r.top && y < r.bottom
        })
    }
}

/// One indicator window per taskbar (primary and every secondary monitor).
struct Bar {
    hwnd: HWND,
    taskbar: HWND,
    view: Option<View>,
    hover: Option<usize>,
    tracking: bool,
}

struct App {
    ctl: HWND,
    bars: Vec<Bar>,
    vdm: Option<IVirtualDesktopManager>,
    settings: Settings,
    desktops: Desktops,
    flashing: Vec<isize>,
    anim: Slide,
    tray_icon: HICON,
    tray_key: Option<(usize, usize, Theme)>,
    shell_msg: u32,
    taskbar_created_msg: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state; silently skips if the state is already borrowed
/// (re-entrant messages during modal loops such as the context menu).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn taskbar() -> Option<HWND> {
    unsafe {
        FindWindowW(w!("Shell_TrayWnd"), None)
            .ok()
            .filter(|h| !h.is_invalid())
    }
}

/// Primary taskbar followed by the taskbars on other monitors ("Show my taskbar on all displays").
fn taskbars() -> Vec<HWND> {
    let mut out: Vec<HWND> = taskbar().into_iter().collect();
    let mut prev: Option<HWND> = None;
    while let Ok(h) = unsafe { FindWindowExW(None, prev, w!("Shell_SecondaryTrayWnd"), None) } {
        if h.is_invalid() {
            break;
        }
        out.push(h);
        prev = Some(h);
    }
    out
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// True when the foreground window covers the whole monitor the taskbar is on (games, video, presentations).
fn fullscreen_app_active(tb: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || fg == tb {
            return false;
        }
        if matches!(
            class_name(fg).as_str(),
            "Progman"
                | "WorkerW"
                | "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "Windows.UI.Core.CoreWindow"
                | "XamlExplorerHostIslandWindow"
        ) {
            return false;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(fg, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
        if cloaked != 0 || !IsWindowVisible(fg).as_bool() {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(MonitorFromWindow(tb, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

impl App {
    fn compute_dots(&self) -> Vec<bool> {
        let mut dots = vec![false; self.desktops.ids.len()];
        if let Some(vdm) = &self.vdm {
            for &h in &self.flashing {
                if let Ok(id) = unsafe { vdm.GetWindowDesktopId(HWND(h as _)) } {
                    if let Some(i) = self.desktops.index_of(&id) {
                        dots[i] = true;
                    }
                }
            }
        }
        dots
    }

    fn compute_view(&self, tb: HWND, hover: Option<usize>, dots: &[bool], theme: Theme) -> View {
        let n = dots.len();
        let mut v = View {
            visible: false,
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            cell: 0,
            gap: 0,
            pad: 0,
            vertical: false,
            highlight: self.anim.pos(),
            scale: 1.0,
            current: self.desktops.current,
            dots: dots.to_vec(),
            hover,
            theme,
        };
        let mut r = RECT::default();
        if unsafe { GetWindowRect(tb, &mut r) }.is_err()
            || !unsafe { IsWindowVisible(tb) }.as_bool()
        {
            return v;
        }
        let dpi = unsafe { GetDpiForWindow(tb) }.max(96);
        let s = dpi as f32 / 96.0;
        v.scale = s;
        let (tw, th) = (r.right - r.left, r.bottom - r.top);
        v.vertical = th > tw;
        // Thickness of the taskbar across its short axis bounds the cell size.
        let thickness = if v.vertical { tw } else { th };
        v.pad = (4.0 * s).round() as i32;
        v.cell = ((26.0 * s).round() as i32)
            .min(thickness - v.pad * 2)
            .max((16.0 * s) as i32);
        v.gap = (4.0 * s).round() as i32;
        let length = v.pad * 2 + n as i32 * v.cell + (n as i32 - 1) * v.gap;
        let breadth = v.cell + v.pad * 2;
        // "Left" means the start of the taskbar: left edge when horizontal, top edge when vertical.
        let start = match self.settings.placement {
            Placement::Left => true,
            Placement::Center => false,
            Placement::Auto => config::taskbar_alignment() != 0,
        };
        let margin = (12.0 * s).round() as i32;
        if v.vertical {
            v.w = breadth;
            v.h = length;
            v.x = r.left + (tw - v.w) / 2;
            v.y = if start {
                r.top + margin
            } else {
                r.top + (th - v.h) / 2
            };
        } else {
            v.w = length;
            v.h = breadth;
            v.x = if start {
                r.left + margin
            } else {
                r.left + (tw - v.w) / 2
            };
            v.y = r.top + (th - v.h) / 2;
        }
        v.visible = !fullscreen_app_active(tb);
        v
    }

    fn draw(&self, bar: HWND, v: &View) {
        let c = render_view(v, self.settings.show_dots);
        present(bar, &c, v.x, v.y);
    }
}

fn render_view(v: &View, show_dots: bool) -> Canvas {
    {
        let s = v.scale;
        let n = v.dots.len();
        let mut c = Canvas::new(v.w, v.h);
        let t = &v.theme;
        let radius = 4.0 * s; // Fluent ControlCornerRadius
        let f = |r: &RECT| (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);

        let mut labels = Vec::with_capacity(n);
        let mut rects = Vec::with_capacity(n);
        for i in 0..n {
            let rect = v.cell_rect(i);
            let (x0, y0, x1, y1) = f(&rect);
            let fill = if v.hover == Some(i) { t.hover } else { t.idle };
            c.round_rect(x0, y0, x1, y1, radius, fill);
            c.round_rect_stroke(
                x0,
                y0,
                x1,
                y1,
                radius,
                s.max(1.0),
                t.stroke_top,
                t.stroke_bottom,
            );
            labels.push((rect, (i + 1).to_string()));
            rects.push(rect);
        }

        // Accent highlight, positioned fractionally so it can slide between cells.
        let off = v.highlight * (v.cell + v.gap) as f32;
        let (hx0, hy0) = if v.vertical {
            (v.pad as f32, v.pad as f32 + off)
        } else {
            (v.pad as f32 + off, v.pad as f32)
        };
        let (hx1, hy1) = (hx0 + v.cell as f32, hy0 + v.cell as f32);
        c.round_rect_glow(hx0, hy0, hx1, hy1, radius, 3.0 * s, t.glow);
        c.round_rect(hx0, hy0, hx1, hy1, radius, t.current);
        c.round_rect_stroke(
            hx0,
            hy0,
            hx1,
            hy1,
            radius,
            s.max(1.0),
            t.accent_stroke_top,
            t.accent_stroke_bottom,
        );
        let hl = RECT {
            left: hx0.round() as i32,
            top: hy0.round() as i32,
            right: hx1.round() as i32,
            bottom: hy1.round() as i32,
        };

        let mask = render::text_mask(v.w, v.h, (12.0 * s).round() as i32, &t.font, &labels);
        for r in &rects {
            c.mask_split(&mask, r, &hl, t.current_text, t.text);
        }
        if show_dots {
            for (i, r) in rects.iter().enumerate() {
                if v.dots[i] && i != v.current {
                    let (cx, cy) = (r.right as f32 - 4.0 * s, r.top as f32 + 4.0 * s);
                    c.circle(cx, cy, 4.0 * s, t.ring);
                    c.circle(cx, cy, 2.8 * s, t.dot);
                }
            }
        }
        c
    }
}

impl App {
    /// Advances the slide animation by one frame, redrawing only the highlight position.
    fn animate(&mut self) {
        let pos = self.anim.pos();
        for i in 0..self.bars.len() {
            if let Some(mut v) = self.bars[i].view.take() {
                v.highlight = pos;
                if v.visible {
                    self.draw(self.bars[i].hwnd, &v);
                }
                self.bars[i].view = Some(v);
            }
        }
        if self.anim.done() {
            unsafe {
                let _ = KillTimer(Some(self.ctl), TIMER_ANIM);
            }
        }
    }

    fn invalidate(&mut self) {
        for b in &mut self.bars {
            b.view = None;
        }
    }

    fn tick(&mut self) {
        // Reconcile bars with the current set of taskbars (monitors added/removed, Explorer restarts).
        let tbs = taskbars();
        self.bars.retain(|b| {
            let keep = tbs.contains(&b.taskbar) && unsafe { IsWindow(Some(b.hwnd)) }.as_bool();
            if !keep {
                unsafe {
                    let _ = DestroyWindow(b.hwnd);
                }
            }
            keep
        });
        for &tb in &tbs {
            if !self.bars.iter().any(|b| b.taskbar == tb) {
                let hwnd = create_bar(tb);
                if !hwnd.is_invalid() {
                    self.bars.push(Bar {
                        hwnd,
                        taskbar: tb,
                        view: None,
                        hover: None,
                        tracking: false,
                    });
                }
            }
        }
        self.desktops = Desktops::read();
        if self.anim.retarget(self.desktops.current) {
            unsafe { SetTimer(Some(self.ctl), TIMER_ANIM, ANIM_FRAME_MS, None) };
        }
        self.flashing
            .retain(|&h| unsafe { IsWindow(Some(HWND(h as _))) }.as_bool());
        let dots = self.compute_dots();
        let theme = Theme::load();

        for i in 0..self.bars.len() {
            let (hwnd, tb, hover) = (self.bars[i].hwnd, self.bars[i].taskbar, self.bars[i].hover);
            // Owned by its taskbar so it stays above it on every virtual desktop.
            if unsafe { GetWindow(hwnd, GW_OWNER) }.ok() != Some(tb) {
                unsafe { SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, tb.0 as isize) };
            }
            let v = self.compute_view(tb, hover, &dots, theme);
            if self.bars[i].view.as_ref() != Some(&v) {
                if v.visible {
                    self.draw(hwnd, &v);
                }
                unsafe {
                    let _ = ShowWindow(
                        hwnd,
                        if v.visible {
                            SW_SHOWNOACTIVATE
                        } else {
                            SW_HIDE
                        },
                    );
                }
            }
            if v.visible {
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                    );
                }
            }
            self.bars[i].view = Some(v);
        }
        self.update_tray(&theme);
    }

    fn update_tray(&mut self, theme: &Theme) {
        let key = (self.desktops.current, self.desktops.ids.len(), *theme);
        if self.tray_key == Some(key) {
            return;
        }
        let dpi = unsafe { GetDpiForWindow(self.ctl) }.max(96);
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.max(16);
        let s = size as f32;
        let mut c = Canvas::new(size, size);
        c.round_rect(0.0, 0.0, s, s, s * 0.22, theme.current);
        let label = (key.0 + 1).to_string();
        let full = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        let mask = render::text_mask(
            size,
            size,
            (s * if label.len() > 1 { 0.62 } else { 0.78 }) as i32,
            &theme.font,
            &[(full, label)],
        );
        c.mask(&mask, &full, theme.current_text);
        let Ok(icon) = c.to_icon() else { return };

        let mut nid = self.nid();
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.hIcon = icon;
        let tip = format!("Virtual desktop {} of {}", key.0 + 1, key.1);
        for (d, s) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *d = s;
        }
        unsafe {
            if self.tray_key.is_none() || !Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
                let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            }
            if !self.tray_icon.is_invalid() {
                let _ = DestroyIcon(self.tray_icon);
            }
        }
        self.tray_icon = icon;
        self.tray_key = Some(key);
    }

    fn nid(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.ctl,
            uID: 1,
            uCallbackMessage: WM_TRAY,
            ..Default::default()
        }
    }

    fn on_shell(&mut self, code: usize, hwnd: isize) {
        match code {
            HSHELL_FLASH => {
                if !self.flashing.contains(&hwnd) {
                    self.flashing.push(hwnd);
                }
            }
            HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED | HSHELL_WINDOWDESTROYED => {
                self.flashing.retain(|&h| h != hwnd);
            }
            _ => return,
        }
        self.tick();
    }
}

fn present(hwnd: HWND, c: &Canvas, x: i32, y: i32) {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        if let Ok(bmp) = c.to_dib(Some(mem)) {
            let old = SelectObject(mem, bmp.into());
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                hwnd,
                Some(screen),
                Some(&POINT { x, y }),
                Some(&SIZE { cx: c.w, cy: c.h }),
                Some(mem),
                Some(&POINT { x: 0, y: 0 }),
                Default::default(),
                Some(&blend),
                ULW_ALPHA,
            );
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}

fn create_bar(owner: HWND) -> HWND {
    unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("DesktopIndicatorBar"),
            w!("Desktop Indicator"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            Some(owner),
            None,
            GetModuleHandleW(None).ok().map(|h| h.into()),
            None,
        )
        .unwrap_or_default()
    }
}

fn show_menu() {
    let Some((ctl, settings, desk)) = with_app(|a| (a.ctl, a.settings, a.desktops.clone())) else {
        return;
    };
    let startup = config::startup_enabled();
    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let Ok(place) = CreatePopupMenu() else { return };
        let check = |b: bool| if b { MF_CHECKED } else { MF_UNCHECKED };
        let header: Vec<u16> = format!("Desktop {} of {}", desk.current + 1, desk.ids.len())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(header.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(
            menu,
            MF_STRING | check(settings.show_dots),
            CMD_DOTS,
            w!("Show notification dots"),
        );
        for (id, p, label) in [
            (
                CMD_PLACE_AUTO,
                Placement::Auto,
                w!("Automatic (opposite taskbar icons)"),
            ),
            (CMD_PLACE_LEFT, Placement::Left, w!("Start (left / top)")),
            (CMD_PLACE_CENTER, Placement::Center, w!("Centre")),
        ] {
            let _ = AppendMenuW(place, MF_STRING | check(settings.placement == p), id, label);
        }
        let _ = AppendMenuW(menu, MF_POPUP, place.0 as usize, w!("Position"));
        let _ = AppendMenuW(
            menu,
            MF_STRING | check(startup),
            CMD_STARTUP,
            w!("Start with Windows"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(ctl);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            ctl,
            None,
        );
        let _ = PostMessageW(Some(ctl), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0 as usize
    };
    match cmd {
        CMD_EXIT => unsafe {
            let _ = DestroyWindow(ctl);
        },
        CMD_STARTUP => config::set_startup(!startup),
        CMD_DOTS | CMD_PLACE_AUTO | CMD_PLACE_LEFT | CMD_PLACE_CENTER => {
            with_app(|a| {
                match cmd {
                    CMD_DOTS => a.settings.show_dots = !a.settings.show_dots,
                    CMD_PLACE_AUTO => a.settings.placement = Placement::Auto,
                    CMD_PLACE_LEFT => a.settings.placement = Placement::Left,
                    _ => a.settings.placement = Placement::Center,
                }
                a.settings.save();
                a.invalidate();
                a.tick();
            });
        }
        _ => {}
    }
}

unsafe extern "system" fn ctl_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_ANIM => {
            with_app(|a| a.animate());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_TICK => {
            with_app(|a| a.tick());
            LRESULT(0)
        }
        WM_TRAY => {
            let event = (lp.0 & 0xFFFF) as u32;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                show_menu();
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_DPICHANGED => {
            with_app(|a| {
                a.invalidate();
                a.tray_key = None;
                a.tick();
            });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DESTROY => {
            with_app(|a| {
                let _ = Shell_NotifyIconW(NIM_DELETE, &a.nid());
                let _ = DeregisterShellHookWindow(a.ctl);
                for b in a.bars.drain(..) {
                    let _ = DestroyWindow(b.hwnd);
                }
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            let handled = with_app(|a| {
                if msg == a.shell_msg {
                    a.on_shell(wp.0, lp.0);
                    true
                } else if msg == a.taskbar_created_msg {
                    // Explorer restarted: re-add the tray icon and re-attach to the new taskbar.
                    a.tray_key = None;
                    a.invalidate();
                    a.tick();
                    true
                } else {
                    false
                }
            });
            if handled == Some(true) {
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
    }
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (mx, my) = (
        (lp.0 & 0xFFFF) as i16 as i32,
        ((lp.0 >> 16) & 0xFFFF) as i16 as i32,
    );
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            with_app(|a| {
                let Some(b) = a.bars.iter_mut().find(|b| b.hwnd == hwnd) else {
                    return;
                };
                if !b.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    b.tracking = TrackMouseEvent(&mut tme).is_ok();
                }
                let hit = b.view.as_ref().and_then(|v| v.hit(mx, my));
                if hit != b.hover {
                    b.hover = hit;
                    a.tick();
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|a| {
                if let Some(b) = a.bars.iter_mut().find(|b| b.hwnd == hwnd) {
                    b.tracking = false;
                    b.hover = None;
                }
                a.tick();
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_app(|a| {
                let bar = a.bars.iter().find(|b| b.hwnd == hwnd);
                if let Some(i) = bar
                    .and_then(|b| b.view.as_ref())
                    .and_then(|v| v.hit(mx, my))
                {
                    desktops::switch(a.desktops.current, i);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            with_app(|a| {
                let cur = a.desktops.current;
                let target = if delta > 0 {
                    cur.saturating_sub(1)
                } else {
                    (cur + 1).min(a.desktops.ids.len() - 1)
                };
                desktops::switch(cur, target);
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_menu();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _mutex = CreateMutexW(None, true, w!("Local\\DesktopIndicator.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let cursor = LoadCursorW(None, IDC_HAND).unwrap_or_default();
        for (name, proc_, cur) in [
            (
                w!("DesktopIndicatorCtl"),
                ctl_proc as WndProcFn,
                HCURSOR::default(),
            ),
            (w!("DesktopIndicatorBar"), bar_proc as WndProcFn, cursor),
        ] {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: instance.into(),
                lpszClassName: name,
                hCursor: cur,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        let Ok(ctl) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("DesktopIndicatorCtl"),
            w!("Desktop Indicator"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            return;
        };
        let vdm: Option<IVirtualDesktopManager> =
            CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok();
        let _ = RegisterShellHookWindow(ctl);

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                ctl,
                bars: Vec::new(),
                vdm,
                settings: Settings::load(),
                desktops: Desktops::read(),
                flashing: Vec::new(),
                anim: Slide::default(),
                tray_icon: HICON::default(),
                tray_key: None,
                shell_msg: RegisterWindowMessageW(w!("SHELLHOOK")),
                taskbar_created_msg: RegisterWindowMessageW(w!("TaskbarCreated")),
            })
        });
        with_app(|a| a.tick());
        SetTimer(Some(ctl), TIMER_TICK, TICK_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Eased slide of the current-desktop highlight between cells.
struct Slide {
    from: f32,
    to: usize,
    start: Option<std::time::Instant>,
}

impl Default for Slide {
    fn default() -> Self {
        Slide {
            from: f32::NAN,
            to: 0,
            start: None,
        }
    }
}

impl Slide {
    fn progress(&self) -> f32 {
        self.start.map_or(1.0, |s| {
            (s.elapsed().as_secs_f32() / ANIM_DURATION).min(1.0)
        })
    }

    fn pos(&self) -> f32 {
        if self.from.is_nan() {
            return self.to as f32;
        }
        let ease = 1.0 - (1.0 - self.progress()).powi(3); // ease-out cubic
        self.from + (self.to as f32 - self.from) * ease
    }

    fn done(&self) -> bool {
        self.progress() >= 1.0
    }

    /// Points the slide at `to`; returns true when a new animation started.
    fn retarget(&mut self, to: usize) -> bool {
        if self.from.is_nan() {
            self.to = to;
            self.from = to as f32;
            return false;
        }
        if to == self.to {
            return false;
        }
        self.from = self.pos();
        self.to = to;
        self.start = Some(std::time::Instant::now());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(vertical: bool) -> View {
        View {
            visible: true,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            cell: 20,
            gap: 4,
            pad: 4,
            vertical,
            highlight: 0.0,
            scale: 1.0,
            current: 0,
            dots: vec![false; 3],
            hover: None,
            theme: Theme::load(),
        }
    }

    #[test]
    fn horizontal_layout_hits_along_x() {
        let v = view(false);
        assert_eq!(v.cell_rect(1).left, 28);
        assert_eq!(v.cell_rect(1).top, 4);
        assert_eq!(v.hit(30, 10), Some(1));
        assert_eq!(v.hit(26, 10), None); // gap
        assert_eq!(v.hit(10, 30), None);
    }

    #[test]
    fn vertical_layout_hits_along_y() {
        let v = view(true);
        assert_eq!(v.cell_rect(2).top, 52);
        assert_eq!(v.cell_rect(2).left, 4);
        assert_eq!(v.hit(10, 55), Some(2));
        assert_eq!(v.hit(55, 10), None);
    }

    /// Renders sample states over a taskbar-coloured background:
    /// `cargo test --release preview -- --ignored` writes `%TEMP%\desktop-indicator-preview.bmp`.
    #[test]
    #[ignore]
    fn preview() {
        let s = 2.0;
        let mk = |highlight: f32, hover: Option<usize>, dots: Vec<bool>| View {
            visible: true,
            x: 0,
            y: 0,
            w: (4.0 * s) as i32 * 2 + 3 * (26.0 * s) as i32 + 2 * (4.0 * s) as i32,
            h: (26.0 * s) as i32 + (4.0 * s) as i32 * 2,
            cell: (26.0 * s) as i32,
            gap: (4.0 * s) as i32,
            pad: (4.0 * s) as i32,
            vertical: false,
            highlight,
            scale: s,
            current: highlight.round() as usize,
            dots,
            hover,
            theme: Theme::load(),
        };
        let states = [
            mk(0.0, None, vec![false; 3]),
            mk(0.5, None, vec![false; 3]),
            mk(0.0, Some(1), vec![false, false, true]),
            {
                // Same states with a saturated (default blue) accent for comparison.
                let mut v = mk(1.0, None, vec![true, false, false]);
                v.theme.current = render::Rgba(0x99, 0xEB, 0xFF, 1.0);
                v.theme.glow = theme::accent_glow(v.theme.current, false);
                v
            },
        ];
        let (w, h) = (states[0].w, states[0].h);
        let rows = states.len() as i32;
        let bg = [0x1C_u8, 0x20, 0x28];
        let mut out = vec![0u8; (w * h * rows * 3) as usize];
        for (row, v) in states.iter().enumerate() {
            let c = render_view(v, true);
            for y in 0..h {
                for x in 0..w {
                    let p = c.px[(y * w + x) as usize];
                    let px = |i: usize, b: u8| {
                        ((p[i] + (1.0 - p[3]) * b as f32 / 255.0) * 255.0).round() as u8
                    };
                    let (r, g, b) = (px(0, bg[0]), px(1, bg[1]), px(2, bg[2]));
                    // Bottom-up BMP rows.
                    let yy = rows * h - 1 - (row as i32 * h + y);
                    let o = ((yy * w + x) * 3) as usize;
                    out[o..o + 3].copy_from_slice(&[b, g, r]);
                }
            }
        }
        let stride = (w * 3 + 3) & !3;
        let mut bmp = Vec::new();
        let size = 54 + stride * h * rows;
        bmp.extend_from_slice(b"BM");
        for v in [size as u32, 0, 54, 40, w as u32, (h * rows) as u32] {
            bmp.extend_from_slice(&v.to_le_bytes());
        }
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&[0u8; 24]);
        for y in 0..h * rows {
            let row = &out[(y * w * 3) as usize..((y + 1) * w * 3) as usize];
            bmp.extend_from_slice(row);
            bmp.extend(std::iter::repeat_n(0, (stride - w * 3) as usize));
        }
        std::fs::write(
            std::env::temp_dir().join("desktop-indicator-preview.bmp"),
            bmp,
        )
        .unwrap();
    }
}
