//! Resolves colours and font from the live Windows settings so the indicator matches the taskbar.

use windows::core::PCWSTR;
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject, GetSysColor, GetTextFaceW,
    SelectObject, CLIP_DEFAULT_PRECIS, COLOR_BTNFACE, COLOR_BTNTEXT, COLOR_HIGHLIGHT,
    COLOR_HIGHLIGHTTEXT, COLOR_HOTLIGHT, COLOR_WINDOW, DEFAULT_CHARSET, DEFAULT_QUALITY,
    OUT_DEFAULT_PRECIS, SYS_COLOR_INDEX,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, NONCLIENTMETRICSW, SPI_GETHIGHCONTRAST, SPI_GETNONCLIENTMETRICS,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::config::{read_binary, read_dword};
use crate::render::Rgba;

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
pub const FONT_LEN: usize = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub light: bool,
    pub current: Rgba,
    pub current_text: Rgba,
    pub idle: Rgba,
    pub hover: Rgba,
    pub text: Rgba,
    pub dot: Rgba,
    /// Approximates the taskbar background; used to outline the notification dot.
    pub ring: Rgba,
    /// Fluent "elevation" border on idle cells, fading from top to bottom.
    pub stroke_top: Rgba,
    pub stroke_bottom: Rgba,
    /// Elevation border on the current (accent) cell.
    pub accent_stroke_top: Rgba,
    pub accent_stroke_bottom: Rgba,
    /// Soft halo around the current cell.
    pub glow: Rgba,
    /// Null-terminated UTF-16 face name.
    pub font: [u16; FONT_LEN],
}

fn rgb(c: [u8; 3]) -> Rgba {
    Rgba(c[0], c[1], c[2], 1.0)
}

fn sys(index: SYS_COLOR_INDEX) -> Rgba {
    let c = unsafe { GetSysColor(index) };
    Rgba(
        (c & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        ((c >> 16) & 0xFF) as u8,
        1.0,
    )
}

/// Black or white, whichever is more readable on `bg` (WCAG relative luminance).
fn contrast_text(bg: Rgba) -> Rgba {
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * lin(bg.0) + 0.7152 * lin(bg.1) + 0.0722 * lin(bg.2);
    if (l + 0.05) / 0.05 > 1.05 / (l + 0.05) {
        Rgba(0, 0, 0, 1.0)
    } else {
        Rgba(255, 255, 255, 1.0)
    }
}

fn high_contrast() -> bool {
    let mut hc = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some(&mut hc as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && hc.dwFlags.contains(HCF_HIGHCONTRASTON)
    }
}

/// AccentPalette holds 8 RGBA entries: Light3, Light2, Light1, Base, Dark1, Dark2, Dark3, unused.
fn accent_palette() -> [[u8; 3]; 7] {
    if let Some(p) = read_binary(ACCENT, "AccentPalette").filter(|p| p.len() >= 28) {
        return std::array::from_fn(|i| [p[i * 4], p[i * 4 + 1], p[i * 4 + 2]]);
    }
    // No palette yet (fresh profile): derive shades from the DWM colorization colour.
    let mut c = 0u32;
    let mut opaque = Default::default();
    let base = if unsafe { DwmGetColorizationColor(&mut c, &mut opaque) }.is_ok() {
        [(c >> 16) as u8, (c >> 8) as u8, c as u8]
    } else {
        [0x00, 0x78, 0xD4]
    };
    let mix = |t: f32, to: f32| base.map(|v| (v as f32 + (to - v as f32) * t).round() as u8);
    [
        mix(0.6, 255.0),
        mix(0.4, 255.0),
        mix(0.2, 255.0),
        base,
        mix(0.2, 0.0),
        mix(0.4, 0.0),
        mix(0.6, 0.0),
    ]
}

fn to_face(name: &[u16]) -> [u16; FONT_LEN] {
    let mut out = [0u16; FONT_LEN];
    for (o, c) in out
        .iter_mut()
        .take(FONT_LEN - 1)
        .zip(name.iter().take_while(|&&c| c != 0))
    {
        *o = *c;
    }
    out
}

fn face_installed(face: &[u16; FONT_LEN]) -> bool {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let font = CreateFontW(
            -12,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            0,
            PCWSTR(face.as_ptr()),
        );
        let old = SelectObject(dc, font.into());
        let mut got = [0u16; FONT_LEN];
        let n = GetTextFaceW(dc, Some(&mut got)) as usize;
        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(dc);
        let want: Vec<u16> = face.iter().copied().take_while(|&c| c != 0).collect();
        String::from_utf16_lossy(&got[..n.min(FONT_LEN)])
            .trim_end_matches('\0')
            .eq_ignore_ascii_case(&String::from_utf16_lossy(&want))
    }
}

/// The Windows UI font (Settings / NONCLIENTMETRICS). Windows 11's shell renders the default
/// "Segoe UI" as its optical-size variant "Segoe UI Variable", so prefer that when available.
fn system_font() -> [u16; FONT_LEN] {
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let face = unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()
    .map(|_| to_face(&ncm.lfMessageFont.lfFaceName))
    .filter(|f| f[0] != 0)
    .unwrap_or_else(|| to_face(&"Segoe UI".encode_utf16().collect::<Vec<_>>()));

    if String::from_utf16_lossy(&face).trim_end_matches('\0') == "Segoe UI" {
        let variable = to_face(&"Segoe UI Variable Text".encode_utf16().collect::<Vec<_>>());
        if face_installed(&variable) {
            return variable;
        }
    }
    face
}

/// A halo only flatters saturated accents; neutral (e.g. grey) accents get none.
pub fn accent_glow(current: Rgba, light: bool) -> Rgba {
    let mx = current.0.max(current.1).max(current.2);
    let mn = current.0.min(current.1).min(current.2);
    let sat = if mx == 0 {
        0.0
    } else {
        (mx - mn) as f32 / mx as f32
    };
    let strength = ((sat - 0.15) / 0.5).clamp(0.0, 1.0);
    Rgba(
        current.0,
        current.1,
        current.2,
        strength * if light { 0.22 } else { 0.32 },
    )
}

impl Theme {
    pub fn load() -> Self {
        let font = system_font();
        let light = read_dword(PERSONALIZE, "SystemUsesLightTheme").unwrap_or(0) != 0;

        if high_contrast() {
            let current = sys(COLOR_HIGHLIGHT);
            return Theme {
                light,
                current,
                current_text: sys(COLOR_HIGHLIGHTTEXT),
                idle: sys(COLOR_BTNFACE),
                hover: sys(COLOR_HOTLIGHT),
                text: sys(COLOR_BTNTEXT),
                dot: current,
                ring: sys(COLOR_WINDOW),
                stroke_top: sys(COLOR_BTNTEXT),
                stroke_bottom: sys(COLOR_BTNTEXT),
                accent_stroke_top: sys(COLOR_HIGHLIGHTTEXT),
                accent_stroke_bottom: sys(COLOR_HIGHLIGHTTEXT),
                glow: Rgba(0, 0, 0, 0.0),
                font,
            };
        }

        let pal = accent_palette();
        // "Show accent colour on Start and taskbar" (only applies in dark mode on Windows 11).
        let accent_taskbar = !light && read_dword(PERSONALIZE, "ColorPrevalence").unwrap_or(0) != 0;
        // Windows 11 uses Light2 for accent fills in dark mode and Dark1 in light mode.
        let current = rgb(if light { pal[4] } else { pal[1] });
        // Values follow the WinUI Fluent control fill / elevation border tokens.
        let (text, idle, hover, stroke_top, stroke_bottom) = if light {
            (
                Rgba(0, 0, 0, 0.9),
                Rgba(255, 255, 255, 0.55),
                Rgba(255, 255, 255, 0.85),
                Rgba(0, 0, 0, 0.06),
                Rgba(0, 0, 0, 0.14),
            )
        } else {
            (
                Rgba(255, 255, 255, 0.92),
                Rgba(255, 255, 255, 0.055),
                Rgba(255, 255, 255, 0.10),
                Rgba(255, 255, 255, 0.10),
                Rgba(255, 255, 255, 0.04),
            )
        };
        let ring = if accent_taskbar {
            rgb(pal[5])
        } else if light {
            Rgba(243, 243, 243, 1.0)
        } else {
            Rgba(32, 32, 32, 1.0)
        };
        Theme {
            light,
            current,
            current_text: contrast_text(current),
            idle,
            hover,
            text,
            // Matches the Windows 11 taskbar "needs attention" highlight.
            dot: Rgba(0xF7, 0x63, 0x0C, 1.0),
            ring,
            stroke_top,
            stroke_bottom,
            accent_stroke_top: Rgba(255, 255, 255, 0.10),
            accent_stroke_bottom: Rgba(0, 0, 0, 0.16),
            glow: accent_glow(current, light),

            font,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_picks_readable_text() {
        assert_eq!(contrast_text(Rgba(0x99, 0xEB, 0xFF, 1.0)).0, 0);
        assert_eq!(contrast_text(Rgba(0x00, 0x5A, 0x9E, 1.0)).0, 255);
    }

    #[test]
    fn loads_theme_with_font() {
        let t = Theme::load();
        assert_ne!(t.font[0], 0);
        assert_eq!(t.current.3, 1.0);
    }
}
