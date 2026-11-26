use std::ffi::c_void;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use arboard::{Clipboard, SetExtApple};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::CFString;
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, EventField,
    KeyCode, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};

use crate::{button_edges, Error};

const POINTER_BURST_GAP: Duration = Duration::from_millis(100);
const CLICK_GAP: Duration = Duration::from_millis(500);
const CLICK_DISTANCE_SQUARED: f64 = 25.0;
static POINTER_TARGET: Mutex<Option<PointerTarget>> = Mutex::new(None);
static CLICK_TRACKER: Mutex<Option<ClickTracker>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct PointerTarget {
    position: CGPoint,
    updated_at: Instant,
    bounds: Option<CGRect>,
}

#[derive(Clone, Copy)]
struct ClickTracker {
    bit: u8,
    count: i64,
    position: CGPoint,
    updated_at: Instant,
    active_bit: u8,
    active_count: i64,
    dragged: bool,
}

impl PointerTarget {
    fn next(
        current: CGPoint,
        previous: Option<Self>,
        now: Instant,
        bounds: Option<CGRect>,
        dx: i32,
        dy: i32,
    ) -> Self {
        // ponytail: remote input owns sub-100ms bursts; add a hardware event tap to merge simultaneous local input.
        let (mut position, bounds) = match previous {
            Some(target) if now.duration_since(target.updated_at) < POINTER_BURST_GAP => {
                (target.position, target.bounds)
            }
            _ => (current, bounds),
        };
        position.x += f64::from(dx);
        position.y += f64::from(dy);
        if let Some(bounds) = bounds {
            position.x = position
                .x
                .clamp(bounds.origin.x, bounds.origin.x + bounds.size.width - 0.001);
            position.y = position.y.clamp(
                bounds.origin.y,
                bounds.origin.y + bounds.size.height - 0.001,
            );
        }
        Self {
            position,
            updated_at: now,
            bounds,
        }
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    static kAXTrustedCheckOptionPrompt: core_foundation::string::CFStringRef;
}

#[link(name = "System", kind = "dylib")]
extern "C" {
    fn pthread_main_np() -> i32;
    static mut _dispatch_main_q: u8;
    fn dispatch_sync_f(queue: *mut c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

pub(crate) fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

pub(crate) fn prompt_accessibility() {
    let _ = ax_prompt();
}

fn ax_prompt() -> bool {
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let val = CFBoolean::true_value();
        let dict = CFDictionary::from_CFType_pairs(&[(key, val)]);
        AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef()) != 0
    }
}

pub(crate) fn open_accessibility_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility")
        .spawn();
}

fn source() -> Result<CGEventSource, Error> {
    thread_local! {
        static SOURCE: std::cell::RefCell<Option<CGEventSource>> =
            const { std::cell::RefCell::new(None) };
    }
    SOURCE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                    .map_err(|_| Error)?,
            );
        }
        Ok(slot.as_ref().unwrap().clone())
    })
}

fn post(ev: &CGEvent) {
    ev.post(CGEventTapLocation::HID);
}

fn post_move(ev: &CGEvent) {
    ev.set_flags(ev.get_flags() | CGEventFlags::CGEventFlagNonCoalesced);
    post(ev);
}

fn on_main<T, F>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if unsafe { pthread_main_np() != 0 } {
        return f();
    }
    let (tx, rx) = mpsc::sync_channel(1);
    dispatch_sync_main(move || {
        let _ = tx.send(f());
    });
    rx.recv().expect("main inject")
}

extern "C" fn dispatch_work(ctx: *mut c_void) {
    let boxed: Box<Box<dyn FnOnce() + Send>> = unsafe { Box::from_raw(ctx as *mut _) };
    boxed();
}

fn dispatch_sync_main<F: FnOnce() + Send + 'static>(f: F) {
    let wrapped: Box<dyn FnOnce() + Send> = Box::new(f);
    let ctx = Box::into_raw(Box::new(wrapped)) as *mut c_void;
    unsafe {
        dispatch_sync_f(
            std::ptr::addr_of_mut!(_dispatch_main_q) as *mut c_void,
            ctx,
            dispatch_work,
        );
    }
}

fn post_key(code: CGKeyCode, down: bool, flags: CGEventFlags) -> Result<(), Error> {
    let ev = CGEvent::new_keyboard_event(source()?, code, down).map_err(|_| Error)?;
    ev.set_flags(flags);
    post(&ev);
    Ok(())
}

fn modifier_code(name: &str) -> Option<CGKeyCode> {
    match name {
        "Shift" => Some(KeyCode::SHIFT),
        "Control" => Some(KeyCode::CONTROL),
        "Alt" => Some(KeyCode::OPTION),
        "Meta" => Some(KeyCode::COMMAND),
        _ => None,
    }
}

fn flags_for(modifiers: &[&str]) -> CGEventFlags {
    let mut f = CGEventFlags::CGEventFlagNull;
    for m in modifiers {
        f |= match *m {
            "Shift" => CGEventFlags::CGEventFlagShift,
            "Control" => CGEventFlags::CGEventFlagControl,
            "Alt" => CGEventFlags::CGEventFlagAlternate,
            "Meta" => CGEventFlags::CGEventFlagCommand,
            _ => CGEventFlags::CGEventFlagNull,
        };
    }
    f
}

pub(crate) fn keycode(key: &str) -> Option<CGKeyCode> {
    Some(match key {
        "Escape" => KeyCode::ESCAPE,
        "Enter" => KeyCode::RETURN,
        "Tab" => KeyCode::TAB,
        "Backspace" => KeyCode::DELETE,
        "Space" => KeyCode::SPACE,
        "z" | "Z" => KeyCode::ANSI_Z,
        "v" | "V" => KeyCode::ANSI_V,
        "c" | "C" => KeyCode::ANSI_C,
        "x" | "X" => KeyCode::ANSI_X,
        "a" | "A" => KeyCode::ANSI_A,
        other if other.chars().count() == 1 => {
            let c = other.chars().next()?.to_ascii_lowercase();
            return letter_code(c);
        }
        _ => return None,
    })
}

fn letter_code(c: char) -> Option<CGKeyCode> {
    Some(match c {
        'a' => KeyCode::ANSI_A,
        'b' => KeyCode::ANSI_B,
        'c' => KeyCode::ANSI_C,
        'd' => KeyCode::ANSI_D,
        'e' => KeyCode::ANSI_E,
        'f' => KeyCode::ANSI_F,
        'g' => KeyCode::ANSI_G,
        'h' => KeyCode::ANSI_H,
        'i' => KeyCode::ANSI_I,
        'j' => KeyCode::ANSI_J,
        'k' => KeyCode::ANSI_K,
        'l' => KeyCode::ANSI_L,
        'm' => KeyCode::ANSI_M,
        'n' => KeyCode::ANSI_N,
        'o' => KeyCode::ANSI_O,
        'p' => KeyCode::ANSI_P,
        'q' => KeyCode::ANSI_Q,
        'r' => KeyCode::ANSI_R,
        's' => KeyCode::ANSI_S,
        't' => KeyCode::ANSI_T,
        'u' => KeyCode::ANSI_U,
        'v' => KeyCode::ANSI_V,
        'w' => KeyCode::ANSI_W,
        'x' => KeyCode::ANSI_X,
        'y' => KeyCode::ANSI_Y,
        'z' => KeyCode::ANSI_Z,
        _ => return None,
    })
}

pub(crate) fn inject_key(key: &str, modifiers: &[&str]) -> Result<(), Error> {
    let key = key.to_string();
    let modifiers: Vec<String> = modifiers.iter().map(|s| (*s).to_string()).collect();
    on_main(move || inject_key_raw(&key, &modifiers))
}

fn inject_key_raw(key: &str, modifiers: &[String]) -> Result<(), Error> {
    let mods: Vec<&str> = modifiers.iter().map(|s| s.as_str()).collect();
    let code = keycode(key).ok_or(Error)?;
    let flags = flags_for(&mods);
    for m in &mods {
        if let Some(mc) = modifier_code(m) {
            post_key(mc, true, CGEventFlags::CGEventFlagNull)?;
        }
    }
    post_key(code, true, flags)?;
    post_key(code, false, flags)?;
    for m in mods.iter().rev() {
        if let Some(mc) = modifier_code(m) {
            post_key(mc, false, CGEventFlags::CGEventFlagNull)?;
        }
    }
    Ok(())
}

fn set_clipboard_excluded(cb: &mut Clipboard, text: &str) -> Result<(), Error> {
    cb.set()
        .exclude_from_history()
        .text(text.to_owned())
        .map_err(|_| Error)
}

pub(crate) fn inject_text(text: &str) -> Result<(), Error> {
    let text = text.to_string();
    on_main(move || {
        let mut cb = Clipboard::new().map_err(|_| Error)?;
        let old = cb.get_text().ok();
        set_clipboard_excluded(&mut cb, &text)?;
        inject_key_raw("v", &["Meta".into()])?;
        std::thread::sleep(Duration::from_millis(80));
        // Only restore or clear when the clipboard still holds our temporary text after pasting.
        if cb.get_text().ok().as_deref() == Some(text.as_str()) {
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
    })
}

fn cursor() -> Result<CGPoint, Error> {
    Ok(CGEvent::new(source()?).map_err(|_| Error)?.location())
}

fn desktop_bounds() -> Option<CGRect> {
    let displays = CGDisplay::active_displays().ok()?;
    let first = CGDisplay::new(*displays.first()?).bounds();
    let (mut min_x, mut min_y) = (first.origin.x, first.origin.y);
    let (mut max_x, mut max_y) = (
        first.origin.x + first.size.width,
        first.origin.y + first.size.height,
    );
    for id in &displays[1..] {
        let bounds = CGDisplay::new(*id).bounds();
        min_x = min_x.min(bounds.origin.x);
        min_y = min_y.min(bounds.origin.y);
        max_x = max_x.max(bounds.origin.x + bounds.size.width);
        max_y = max_y.max(bounds.origin.y + bounds.size.height);
    }
    // ponytail: union bounds ignore gaps between staggered displays; keep each rectangle if gap overshoot matters.
    Some(CGRect::new(
        &CGPoint::new(min_x, min_y),
        &CGSize::new(max_x - min_x, max_y - min_y),
    ))
}

fn pointer_position(dx: i32, dy: i32) -> Result<CGPoint, Error> {
    let now = Instant::now();
    let mut target = POINTER_TARGET.lock().map_err(|_| Error)?;
    let (current, bounds) = match *target {
        Some(previous) if now.duration_since(previous.updated_at) < POINTER_BURST_GAP => {
            (previous.position, None)
        }
        _ => (cursor()?, desktop_bounds()),
    };
    let next = PointerTarget::next(current, *target, now, bounds, dx, dy);
    *target = Some(next);
    Ok(next.position)
}

fn click_close_enough(a: CGPoint, b: CGPoint) -> bool {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy <= CLICK_DISTANCE_SQUARED
}

fn click_count_for_down(bit: u8, position: CGPoint) -> i64 {
    let now = Instant::now();
    let mut tracker = CLICK_TRACKER.lock().unwrap_or_else(|e| e.into_inner());
    let count = match *tracker {
        Some(prev)
            if prev.bit == bit
                && now.duration_since(prev.updated_at) <= CLICK_GAP
                && click_close_enough(prev.position, position) =>
        {
            (prev.count + 1).min(3)
        }
        _ => 1,
    };
    *tracker = Some(ClickTracker {
        bit,
        count,
        position,
        updated_at: now,
        active_bit: bit,
        active_count: count,
        dragged: false,
    });
    count
}

fn click_count_for_up(bit: u8) -> i64 {
    let now = Instant::now();
    let mut tracker = CLICK_TRACKER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut current) = *tracker else {
        return 1;
    };
    if current.active_bit != bit {
        return current.count.max(1);
    }
    let count = current.active_count.max(1);
    if current.dragged {
        *tracker = None;
    } else {
        current.updated_at = now;
        current.active_bit = 0;
        current.active_count = 0;
        *tracker = Some(current);
    }
    count
}

fn active_click_count(buttons: u8) -> i64 {
    let tracker = CLICK_TRACKER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(current) = *tracker else {
        return 1;
    };
    if current.active_bit != 0 && buttons & current.active_bit != 0 {
        current.active_count.max(1)
    } else {
        1
    }
}

fn note_drag(buttons: u8) {
    let mut tracker = CLICK_TRACKER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut current) = *tracker {
        if current.active_bit != 0 && buttons & current.active_bit != 0 {
            current.dragged = true;
            *tracker = Some(current);
        }
    }
}

fn set_click_count(ev: &CGEvent, count: i64) {
    ev.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, count);
}

pub(crate) fn inject_pointer(
    dx: i32,
    dy: i32,
    prev: u8,
    buttons: u8,
    wheel: i32,
) -> Result<(), Error> {
    inject_pointer_raw(dx, dy, prev, buttons, wheel)
}

fn inject_pointer_raw(dx: i32, dy: i32, prev: u8, buttons: u8, wheel: i32) -> Result<(), Error> {
    let (downs, ups) = button_edges(prev, buttons);
    let src = source()?;
    let pos = if dx != 0 || dy != 0 || downs != 0 || ups != 0 {
        pointer_position(dx, dy)?
    } else {
        CGPoint::new(0.0, 0.0)
    };

    if dx != 0 || dy != 0 {
        let ty = if buttons & 1 != 0 {
            CGEventType::LeftMouseDragged
        } else if buttons & 2 != 0 {
            CGEventType::RightMouseDragged
        } else {
            CGEventType::MouseMoved
        };
        let ev = CGEvent::new_mouse_event(src.clone(), ty, pos, CGMouseButton::Left)
            .map_err(|_| Error)?;
        if buttons != 0 {
            set_click_count(&ev, active_click_count(buttons));
            note_drag(buttons);
        }
        ev.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, i64::from(dx));
        ev.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, i64::from(dy));
        post_move(&ev);
    }

    for (bit, down_ty, up_ty, btn) in [
        (
            1u8,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGMouseButton::Left,
        ),
        (
            2,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGMouseButton::Right,
        ),
        (
            4,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGMouseButton::Center,
        ),
    ] {
        if downs & bit != 0 {
            let ev = CGEvent::new_mouse_event(src.clone(), down_ty, pos, btn).map_err(|_| Error)?;
            set_click_count(&ev, click_count_for_down(bit, pos));
            post(&ev);
        }
        if ups & bit != 0 {
            let ev = CGEvent::new_mouse_event(src.clone(), up_ty, pos, btn).map_err(|_| Error)?;
            set_click_count(&ev, click_count_for_up(bit));
            post(&ev);
        }
    }

    if wheel != 0 {
        let ev = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 1, wheel, 0, 0)
            .map_err(|_| Error)?;
        post_move(&ev);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static CLICK_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn named_keycodes() {
        assert_eq!(keycode("Escape"), Some(KeyCode::ESCAPE));
        assert_eq!(keycode("Enter"), Some(KeyCode::RETURN));
        assert_eq!(keycode("v"), Some(KeyCode::ANSI_V));
        assert!(keycode("unknown").is_none());
    }

    #[test]
    fn click_count_tracks_double_and_triple_clicks() {
        let _guard = CLICK_TEST_LOCK.lock().unwrap();
        let pos = CGPoint::new(20.0, 40.0);
        *CLICK_TRACKER.lock().unwrap_or_else(|e| e.into_inner()) = None;

        assert_eq!(click_count_for_down(1, pos), 1);
        assert_eq!(click_count_for_up(1), 1);
        assert_eq!(click_count_for_down(1, pos), 2);
        assert_eq!(click_count_for_up(1), 2);
        assert_eq!(click_count_for_down(1, pos), 3);
        assert_eq!(click_count_for_up(1), 3);
    }

    #[test]
    fn drag_resets_click_count() {
        let _guard = CLICK_TEST_LOCK.lock().unwrap();
        let pos = CGPoint::new(20.0, 40.0);
        *CLICK_TRACKER.lock().unwrap_or_else(|e| e.into_inner()) = None;

        assert_eq!(click_count_for_down(1, pos), 1);
        note_drag(1);
        assert_eq!(click_count_for_up(1), 1);
        assert_eq!(click_count_for_down(1, pos), 1);
    }

    #[test]
    fn active_pointer_burst_uses_last_posted_target() {
        let now = Instant::now();
        let bounds = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(200.0, 100.0));
        let first = PointerTarget::next(CGPoint::new(100.0, 50.0), None, now, Some(bounds), 5, 0);
        let second = PointerTarget::next(
            CGPoint::new(100.0, 50.0),
            Some(first),
            now + Duration::from_millis(1),
            None,
            -5,
            0,
        );
        assert_eq!((second.position.x, second.position.y), (100.0, 50.0));

        let after_idle = PointerTarget::next(
            CGPoint::new(220.0, 70.0),
            Some(second),
            second.updated_at + POINTER_BURST_GAP,
            None,
            5,
            0,
        );
        assert_eq!(
            (after_idle.position.x, after_idle.position.y),
            (225.0, 70.0)
        );

        let at_edge = PointerTarget::next(
            CGPoint::new(100.0, 50.0),
            Some(first),
            now + Duration::from_millis(2),
            None,
            500,
            0,
        );
        assert!((at_edge.position.x - 199.999).abs() < 1e-9);
    }
}
