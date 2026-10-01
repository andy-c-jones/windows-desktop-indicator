use windows::core::GUID;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LCONTROL, VK_LEFT, VK_LWIN, VK_RIGHT,
};

use crate::config::read_binary;

pub type DesktopId = [u8; 16];

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Desktops {
    pub ids: Vec<DesktopId>,
    pub current: usize,
}

const VD_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops";

fn split_ids(raw: &[u8]) -> Vec<DesktopId> {
    raw.chunks_exact(16)
        .map(|c| c.try_into().unwrap())
        .collect()
}

fn current_id() -> Option<DesktopId> {
    if let Some(b) = read_binary(VD_KEY, "CurrentVirtualDesktop") {
        if b.len() == 16 {
            return b.try_into().ok();
        }
    }
    // Older builds store the current desktop per logon session.
    let mut session = 0u32;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session).ok()? };
    let key = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{session}\VirtualDesktops"
    );
    read_binary(&key, "CurrentVirtualDesktop").and_then(|b| b.try_into().ok())
}

impl Desktops {
    pub fn read() -> Self {
        let mut ids = read_binary(VD_KEY, "VirtualDesktopIDs")
            .map(|b| split_ids(&b))
            .unwrap_or_default();
        let cur = current_id();
        if ids.is_empty() {
            ids.push(cur.unwrap_or([0; 16]));
        }
        let current = cur
            .and_then(|c| ids.iter().position(|i| *i == c))
            .unwrap_or(0);
        Desktops { ids, current }
    }

    pub fn index_of(&self, id: &GUID) -> Option<usize> {
        let b = guid_bytes(id);
        self.ids.iter().position(|i| *i == b)
    }
}

pub fn guid_bytes(g: &GUID) -> DesktopId {
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&g.data1.to_le_bytes());
    b[4..6].copy_from_slice(&g.data2.to_le_bytes());
    b[6..8].copy_from_slice(&g.data3.to_le_bytes());
    b[8..16].copy_from_slice(&g.data4);
    b
}

fn key(vk: VIRTUAL_KEY, up: bool, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Switches desktops using the built-in Ctrl+Win+Left/Right shortcut, which is
/// stable across Windows builds (unlike the undocumented internal COM API).
pub fn switch(from: usize, to: usize) {
    if from == to {
        return;
    }
    let arrow = if to > from { VK_RIGHT } else { VK_LEFT };
    let steps = from.abs_diff(to);
    let mut inputs = vec![key(VK_LCONTROL, false, false), key(VK_LWIN, false, true)];
    for _ in 0..steps {
        inputs.push(key(arrow, false, true));
        inputs.push(key(arrow, true, true));
    }
    inputs.push(key(VK_LWIN, true, true));
    inputs.push(key(VK_LCONTROL, true, false));
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_registry_desktops() {
        let d = Desktops::read();
        assert!(!d.ids.is_empty() && d.current < d.ids.len());
    }

    #[test]
    fn guid_roundtrip() {
        let g = GUID::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
        let b = guid_bytes(&g);
        assert_eq!(&b[..4], &[0x33, 0x22, 0x11, 0x00]);
        assert_eq!(b[15], 0xff);
    }
}
