use std::ffi::c_void;

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
    DrawTextW, GdiFlush, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, HBITMAP, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

/// Premultiplied BGRA canvas (matches what UpdateLayeredWindow expects).
pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<[f32; 4]>, // premultiplied r, g, b, a in 0..1
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgba(pub u8, pub u8, pub u8, pub f32);

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        Canvas {
            w,
            h,
            px: vec![[0.0; 4]; (w.max(1) * h.max(1)) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: Rgba, coverage: f32) {
        if coverage <= 0.0 || x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = c.3 * coverage.min(1.0);
        let p = &mut self.px[(y * self.w + x) as usize];
        let inv = 1.0 - a;
        p[0] = c.0 as f32 / 255.0 * a + p[0] * inv;
        p[1] = c.1 as f32 / 255.0 * a + p[1] * inv;
        p[2] = c.2 as f32 / 255.0 * a + p[2] * inv;
        p[3] = a + p[3] * inv;
    }

    pub fn round_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, c: Rgba) {
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (hw, hh) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let r = r.min(hw).min(hh);
        for y in y0.floor() as i32..y1.ceil() as i32 {
            for x in x0.floor() as i32..x1.ceil() as i32 {
                let qx = ((x as f32 + 0.5) - cx).abs() - (hw - r);
                let qy = ((y as f32 + 0.5) - cy).abs() - (hh - r);
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let d = outside + qx.max(qy).min(0.0) - r;
                self.blend(x, y, c, 0.5 - d);
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgba) {
        self.round_rect(cx - r, cy - r, cx + r, cy + r, r, c);
    }

    /// Composites `color` through an 8-bit coverage mask, limited to `clip`.
    pub fn mask(&mut self, mask: &[u8], clip: &RECT, c: Rgba) {
        for y in clip.top.max(0)..clip.bottom.min(self.h) {
            for x in clip.left.max(0)..clip.right.min(self.w) {
                let m = mask[(y * self.w + x) as usize];
                if m > 0 {
                    self.blend(x, y, c, m as f32 / 255.0);
                }
            }
        }
    }

    fn to_bgra(&self) -> Vec<u32> {
        self.px
            .iter()
            .map(|p| {
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                (q(p[3]) << 24) | (q(p[0]) << 16) | (q(p[1]) << 8) | q(p[2])
            })
            .collect()
    }

    /// Creates a 32bpp top-down DIB section containing the canvas.
    pub fn to_dib(&self, hdc: Option<HDC>) -> Result<HBITMAP> {
        let (bmp, bits) = dib(hdc, self.w, self.h)?;
        let data = self.to_bgra();
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), bits as *mut u32, data.len()) };
        Ok(bmp)
    }

    pub fn to_icon(&self) -> Result<HICON> {
        unsafe {
            let color = self.to_dib(None)?;
            let mask = CreateBitmap(self.w, self.h, 1, 1, None);
            let info = ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            };
            let icon = CreateIconIndirect(&info);
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
            icon
        }
    }
}

fn dib(hdc: Option<HDC>, w: i32, h: i32) -> Result<(HBITMAP, *mut c_void)> {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let bmp = unsafe { CreateDIBSection(hdc, &bi, DIB_RGB_COLORS, &mut bits, None, 0)? };
    Ok((bmp, bits))
}

/// Renders text labels with grayscale antialiasing and returns an 8-bit coverage mask.
pub fn text_mask(w: i32, h: i32, font_px: i32, face: &[u16], labels: &[(RECT, String)]) -> Vec<u8> {
    let mut out = vec![0u8; (w * h) as usize];
    unsafe {
        let dc = CreateCompatibleDC(None);
        let Ok((bmp, bits)) = dib(Some(dc), w, h) else {
            let _ = DeleteDC(dc);
            return out;
        };
        let old_bmp = SelectObject(dc, bmp.into());
        let font = CreateFontW(
            -font_px,
            0,
            0,
            0,
            400, // FW_NORMAL, matching the taskbar clock
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            PCWSTR(face.as_ptr()),
        );
        let old_font = SelectObject(dc, font.into());
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        SetBkMode(dc, TRANSPARENT);
        for (rect, text) in labels {
            let mut r = *rect;
            let mut t: Vec<u16> = text.encode_utf16().collect();
            DrawTextW(dc, &mut t, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }
        let _ = GdiFlush();
        let src = std::slice::from_raw_parts(bits as *const u32, (w * h) as usize);
        for (o, p) in out.iter_mut().zip(src) {
            let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
            *o = r.max(g).max(b) as u8;
        }
        SelectObject(dc, old_font);
        SelectObject(dc, old_bmp);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_and_text_render() {
        let mut c = Canvas::new(40, 30);
        c.round_rect(4.0, 4.0, 30.0, 26.0, 5.0, Rgba(255, 0, 0, 1.0));
        let centre = c.px[(15 * 40 + 17) as usize];
        assert!((centre[3] - 1.0).abs() < 1e-3 && (centre[0] - 1.0).abs() < 1e-3);
        assert_eq!(c.px[0][3], 0.0);
        let r = RECT {
            left: 4,
            top: 4,
            right: 30,
            bottom: 26,
        };
        let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
        let mask = text_mask(40, 30, 14, &face, &[(r, "2".into())]);
        assert!(mask.iter().filter(|&&m| m > 128).count() > 10);
        c.mask(&mask, &r, Rgba(255, 255, 255, 1.0));
        assert!(c.to_icon().is_ok());
        assert_eq!(c.to_bgra()[(15 * 40 + 1) as usize] >> 24, 0);
    }
}
