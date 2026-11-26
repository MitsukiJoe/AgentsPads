use std::mem::zeroed;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use arboard::{Clipboard, SetExtWindows};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
    VK_BACK, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

use crate::{burst_follow, button_edges, normalize_virtual_abs, Error};

static POINTER_TARGET: Mutex<Option<(Instant, i32, i32)>> = Mutex::new(None);

fn vk(key: &str) -> Option<VIRTUAL_KEY> {
    Some(match key {
        "Escape" => VK_ESCAPE,
        "Enter" => VK_RETURN,
        "Tab" => VK_TAB,
        "Backspace" => VK_BACK,
        "Space" => VK_SPACE,
        other if other.chars().count() == 1 => {
            let c = other.chars().next()?.to_ascii_uppercase();
            if c.is_ascii_alphabetic() {
                VIRTUAL_KEY(c as u16)
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

fn modifier_vk(name: &str) -> Option<VIRTUAL_KEY> {
    match name {
        "Shift" => Some(VK_SHIFT),
        "Control" => Some(VK_CONTROL),
        "Alt" => Some(VK_MENU),
        "Meta" => Some(VK_LWIN),
        _ => None,
    }
}

fn send(inputs: &[INPUT]) -> Result<(), Error> {
    let n = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize == inputs.len() {
        Ok(())
    } else {
        Err(Error)
    }
}

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    Default::default()
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let _ = &mut input;
    input
}

pub(crate) fn inject_key(key: &str, modifiers: &[&str]) -> Result<(), Error> {
    let code = vk(key).ok_or(Error)?;
    let mut seq = Vec::new();
    for m in modifiers {
        if let Some(mv) = modifier_vk(m) {
            seq.push(key_input(mv, false));
        }
    }
    seq.push(key_input(code, false));
    seq.push(key_input(code, true));
    for m in modifiers.iter().rev() {
        if let Some(mv) = modifier_vk(m) {
            seq.push(key_input(mv, true));
        }
    }
    send(&seq)
}

#[link(name = "user32")]
extern "system" {
    fn GetClipboardSequenceNumber() -> u32;
}

fn set_clipboard_excluded(cb: &mut Clipboard, text: &str) -> Result<(), Error> {
    cb.set()
        .exclude_from_monitoring()
        .exclude_from_cloud()
        .exclude_from_history()
        .text(text.to_owned())
        .map_err(|_| Error)
}

pub(crate) fn inject_text(text: &str) -> Result<(), Error> {
    let mut cb = Clipboard::new().map_err(|_| Error)?;
    let old = cb.get_text().ok();
    set_clipboard_excluded(&mut cb, text)?;
    let seq = unsafe { GetClipboardSequenceNumber() };
    inject_key("v", &["Control"])?;
    std::thread::sleep(Duration::from_millis(80));
    // Only restore or clear when no other process modified the clipboard during the paste window.
    if unsafe { GetClipboardSequenceNumber() } == seq {
        match old {
            Some(old) => {
                let _ = set_clipboard_excluded(&mut cb, &old);
            }
            None => {
                let _ = cb.clear();
            }
        }
    }
    Ok(())
}

fn cursor() -> Result<(i32, i32), Error> {
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt) }.map_err(|_| Error)?;
    Ok((pt.x, pt.y))
}

fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

fn next_pos(dx: i32, dy: i32) -> Result<(i32, i32), Error> {
    let now = Instant::now();
    let mut slot = POINTER_TARGET.lock().map_err(|_| Error)?;
    let (vx, vy, vw, vh) = virtual_screen();
    let next = burst_follow(
        *slot,
        now,
        cursor()?,
        dx,
        dy,
        (vx, vy),
        (vx + vw - 1, vy + vh - 1),
    );
    *slot = Some((now, next.0, next.1));
    Ok(next)
}

fn mouse_move_abs(x: i32, y: i32) -> INPUT {
    let (vx, vy, vw, vh) = virtual_screen();
    let (dx, dy) = normalize_virtual_abs(x, y, vx, vy, vw, vh);
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE
                    | MOUSEEVENTF_ABSOLUTE
                    | MOUSEEVENTF_VIRTUALDESK
                    | MOUSEEVENTF_MOVE_NOCOALESCE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_btn(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub(crate) fn inject_pointer(
    dx: i32,
    dy: i32,
    prev: u8,
    buttons: u8,
    wheel: i32,
) -> Result<(), Error> {
    let (downs, ups) = button_edges(prev, buttons);
    let mut seq = Vec::new();
    if dx != 0 || dy != 0 {
        let (x, y) = next_pos(dx, dy)?;
        seq.push(mouse_move_abs(x, y));
    }
    if downs & 1 != 0 {
        seq.push(mouse_btn(MOUSEEVENTF_LEFTDOWN));
    }
    if downs & 2 != 0 {
        seq.push(mouse_btn(MOUSEEVENTF_RIGHTDOWN));
    }
    if downs & 4 != 0 {
        seq.push(mouse_btn(MOUSEEVENTF_MIDDLEDOWN));
    }
    if ups & 1 != 0 {
        seq.push(mouse_btn(MOUSEEVENTF_LEFTUP));
    }
    if ups & 2 != 0 {
        seq.push(mouse_btn(MOUSEEVENTF_RIGHTUP));
    }
    if ups & 4 != 0 {
        seq.push(mouse_btn(MOUSEEVENTF_MIDDLEUP));
    }
    if wheel != 0 {
        seq.push(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: wheel.saturating_mul(12) as u32,
                    dwFlags: MOUSEEVENTF_WHEEL,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    if seq.is_empty() {
        return Ok(());
    }
    send(&seq)
}

#[allow(dead_code)]
fn _keep_zeroed() {
    let _: INPUT = unsafe { zeroed() };
}
