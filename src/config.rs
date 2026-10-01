use std::ffi::c_void;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY, HKEY_CURRENT_USER, REG_DWORD,
    REG_ROUTINE_FLAGS, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};

const APP_KEY: &str = r"Software\DesktopIndicator";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "DesktopIndicator";

fn read_raw(hkey: HKEY, subkey: &str, value: &str, flags: REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    let sk = HSTRING::from(subkey);
    let v = HSTRING::from(value);
    unsafe {
        let mut size = 0u32;
        let rc = RegGetValueW(
            hkey,
            PCWSTR(sk.as_ptr()),
            PCWSTR(v.as_ptr()),
            flags,
            None,
            None,
            Some(&mut size),
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let rc = RegGetValueW(
            hkey,
            PCWSTR(sk.as_ptr()),
            PCWSTR(v.as_ptr()),
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        buf.truncate(size as usize);
        Some(buf)
    }
}

pub fn read_binary(subkey: &str, value: &str) -> Option<Vec<u8>> {
    read_raw(HKEY_CURRENT_USER, subkey, value, RRF_RT_REG_BINARY)
}

pub fn read_dword(subkey: &str, value: &str) -> Option<u32> {
    let b = read_raw(HKEY_CURRENT_USER, subkey, value, RRF_RT_REG_DWORD)?;
    (b.len() >= 4).then(|| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn write_dword(subkey: &str, value: &str, data: u32) {
    let sk = HSTRING::from(subkey);
    let v = HSTRING::from(value);
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sk.as_ptr()),
            PCWSTR(v.as_ptr()),
            REG_DWORD.0,
            Some(&data as *const u32 as *const c_void),
            4,
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// Left of the taskbar when icons are centred, centre when icons are left-aligned.
    Auto = 0,
    Left = 1,
    Center = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub show_dots: bool,
    pub placement: Placement,
}

impl Settings {
    pub fn load() -> Self {
        let show_dots = read_dword(APP_KEY, "ShowNotificationDots").unwrap_or(1) != 0;
        let placement = match read_dword(APP_KEY, "Placement").unwrap_or(0) {
            1 => Placement::Left,
            2 => Placement::Center,
            _ => Placement::Auto,
        };
        Settings {
            show_dots,
            placement,
        }
    }

    pub fn save(&self) {
        write_dword(APP_KEY, "ShowNotificationDots", self.show_dots as u32);
        write_dword(APP_KEY, "Placement", self.placement as u32);
    }
}

pub fn startup_enabled() -> bool {
    read_raw(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ).is_some()
}

pub fn set_startup(enabled: bool) {
    let sk = HSTRING::from(RUN_KEY);
    let v = HSTRING::from(RUN_VALUE);
    unsafe {
        if enabled {
            let exe = std::env::current_exe().unwrap_or_default();
            let cmd: Vec<u16> = format!("\"{}\"", exe.display())
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(sk.as_ptr()),
                PCWSTR(v.as_ptr()),
                REG_SZ.0,
                Some(cmd.as_ptr() as *const c_void),
                (cmd.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR(sk.as_ptr()), PCWSTR(v.as_ptr()));
        }
    }
}
/// 0 = left-aligned taskbar icons, 1 = centred (Windows 11 default).
pub fn taskbar_alignment() -> u32 {
    read_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced",
        "TaskbarAl",
    )
    .unwrap_or(1)
}
