use std::time::Duration;

use crate::protocol::{InMsg, OutMsg};

#[derive(Default)]
pub struct Conn {
    pub shadow: String,
    /// 本连接最近一次已经交给注入侧的按键位。0 表示没按着。
    buttons: u8,
}

/// 连接退出，或暂停期间收到抬起时，补一次全抬起。没按着则什么都不做。
pub(crate) fn release_stuck_pointer(conn: &mut Conn) -> Option<Action> {
    if conn.buttons == 0 {
        return None;
    }
    conn.buttons = 0;
    Some(Action::Pointer {
        dx: 0.0,
        dy: 0.0,
        buttons: 0,
        wheel: 0,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Text(String),
    Enter,
    Undo,
    Key {
        key: String,
        modifiers: Vec<String>,
    },
    Pointer {
        dx: f64,
        dy: f64,
        buttons: u8,
        wheel: i32,
    },
}

pub fn handle(
    paused: bool,
    sync_enabled: bool,
    conn: &mut Conn,
    msg: InMsg,
) -> (Vec<OutMsg>, Vec<Action>) {
    match msg {
        InMsg::Hello { .. } | InMsg::Pair { .. } => (vec![], vec![]),
        InMsg::Ping => (vec![OutMsg::Pong { sync_enabled }], vec![]),
        InMsg::Text {
            content,
            auto_enter,
            send_mode,
        } => {
            if paused {
                return (
                    vec![OutMsg::Ack {
                        ok: false,
                        clear_input: false,
                    }],
                    vec![],
                );
            }
            let actions = apply_text(conn, &content, auto_enter, &send_mode);
            (
                vec![OutMsg::Ack {
                    ok: true,
                    clear_input: send_mode != "shadow",
                }],
                actions,
            )
        }
        InMsg::Key { key, modifiers } => {
            if paused {
                return (
                    vec![OutMsg::Ack {
                        ok: false,
                        clear_input: false,
                    }],
                    vec![],
                );
            }
            (
                vec![OutMsg::Ack {
                    ok: true,
                    clear_input: false,
                }],
                vec![Action::Key { key, modifiers }],
            )
        }
        InMsg::Pointer {
            dx,
            dy,
            buttons,
            wheel,
        } => {
            if paused {
                // 暂停时丢掉移动，但本连接还按着键时必须把 buttons:0 转成抬起。
                let actions = if buttons == 0 {
                    release_stuck_pointer(conn).into_iter().collect()
                } else {
                    Vec::new()
                };
                return (
                    vec![OutMsg::Ack {
                        ok: false,
                        clear_input: false,
                    }],
                    actions,
                );
            }
            conn.buttons = buttons;
            (
                vec![],
                vec![Action::Pointer {
                    dx,
                    dy,
                    buttons,
                    wheel,
                }],
            )
        }
        InMsg::Undo => {
            if paused {
                return (
                    vec![OutMsg::Ack {
                        ok: false,
                        clear_input: false,
                    }],
                    vec![],
                );
            }
            conn.shadow.clear();
            (
                vec![OutMsg::Ack {
                    ok: true,
                    clear_input: false,
                }],
                vec![Action::Undo],
            )
        }
    }
}

fn apply_text(conn: &mut Conn, content: &str, auto_enter: bool, send_mode: &str) -> Vec<Action> {
    match send_mode {
        "shadow" => {
            if let Some(delta) = content.strip_prefix(conn.shadow.as_str()) {
                conn.shadow = content.to_string();
                if delta.is_empty() {
                    vec![]
                } else {
                    vec![Action::Text(delta.to_string())]
                }
            } else {
                vec![]
            }
        }
        "commit" | "submit" => {
            let suffix = content
                .strip_prefix(conn.shadow.as_str())
                .unwrap_or(content);
            conn.shadow.clear();
            let mut out = Vec::new();
            if !suffix.is_empty() {
                out.push(Action::Text(suffix.to_string()));
            }
            if auto_enter {
                out.push(Action::Enter);
            }
            out
        }
        _ => vec![],
    }
}

#[cfg(windows)]
static INPUT_STOPPED: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

#[cfg(any(windows, test))]
fn input_if_running(gate: &std::sync::Mutex<bool>, apply: impl FnOnce()) {
    let stopped = gate.lock().unwrap();
    if !*stopped {
        apply();
    }
}

#[cfg(any(windows, test))]
fn stop_input_once(gate: &std::sync::Mutex<bool>, release: impl FnOnce()) {
    let mut stopped = gate.lock().unwrap();
    if !*stopped {
        *stopped = true;
        release();
    }
}

#[cfg(windows)]
pub fn stop_input() {
    stop_input_once(&INPUT_STOPPED, || {
        let _ = agentpad_input::inject_pointer(0.0, 0.0, 0, 0);
    });
}

pub fn apply_actions(actions: &[Action]) {
    if !actions.is_empty() && !agentpad_input::accessibility_trusted() {
        for action in actions {
            crate::logutil::operation(crate::logutil::action_category(action), true, false);
        }
        return;
    }
    #[cfg(windows)]
    input_if_running(&INPUT_STOPPED, || {
        run_actions(actions, |a| inject_action(a).is_ok());
    });
    #[cfg(not(windows))]
    run_actions(actions, |a| inject_action(a).is_ok());
}

/// 紧跟在文字后的 Enter：文字成功才等待后回车，文字失败则不回车，免得提交目标框里原有内容。
fn run_actions(actions: &[Action], mut inject: impl FnMut(&Action) -> bool) {
    let mut text_ok = None;
    for a in actions {
        if matches!(a, Action::Enter) {
            match text_ok {
                Some(false) => {
                    crate::logutil::operation(crate::logutil::action_category(a), true, false);
                    text_ok = None;
                    continue;
                }
                Some(true) => std::thread::sleep(Duration::from_millis(80)),
                None => {}
            }
        }
        let succeeded = inject(a);
        crate::logutil::operation(crate::logutil::action_category(a), true, succeeded);
        text_ok = matches!(a, Action::Text(_)).then_some(succeeded);
    }
}

fn inject_action(a: &Action) -> Result<(), agentpad_input::Error> {
    match a {
        Action::Text(t) => agentpad_input::inject_text(t),
        Action::Enter => agentpad_input::inject_key("Enter", &[]),
        Action::Undo => agentpad_input::undo(),
        Action::Key { key, modifiers } => {
            let mods: Vec<&str> = modifiers.iter().map(|s| s.as_str()).collect();
            agentpad_input::inject_key(key, &mods)
        }
        Action::Pointer {
            dx,
            dy,
            buttons,
            wheel,
        } => agentpad_input::inject_pointer(*dx, *dy, *buttons, *wheel),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_releases_once_and_rejects_late_queued_input() {
        let gate = std::sync::Mutex::new(false);
        let calls = std::cell::RefCell::new(Vec::new());
        input_if_running(&gate, || calls.borrow_mut().push("press"));
        stop_input_once(&gate, || calls.borrow_mut().push("release"));
        input_if_running(&gate, || calls.borrow_mut().push("late press"));
        stop_input_once(&gate, || calls.borrow_mut().push("second release"));
        assert_eq!(*calls.borrow(), ["press", "release"]);
    }

    #[test]
    fn failed_text_cancels_following_enter_only() {
        let mut sent = Vec::new();
        let actions = [
            Action::Text("a".into()),
            Action::Enter,
            Action::Enter,
            Action::Text("b".into()),
            Action::Enter,
        ];
        run_actions(&actions, |a| {
            sent.push(a.clone());
            !matches!(a, Action::Text(t) if t == "a")
        });
        assert_eq!(
            sent,
            [
                Action::Text("a".into()),
                Action::Enter,
                Action::Text("b".into()),
                Action::Enter,
            ]
        );
        let mut sent = Vec::new();
        run_actions(&[Action::Enter], |a| {
            sent.push(a.clone());
            true
        });
        assert_eq!(sent, [Action::Enter]);
    }

    #[test]
    fn shadow_appends_only() {
        let mut c = Conn::default();
        let (_, a) = handle(
            false,
            true,
            &mut c,
            InMsg::Text {
                content: "hel".into(),
                auto_enter: false,
                send_mode: "shadow".into(),
            },
        );
        assert_eq!(a, vec![Action::Text("hel".into())]);
        let (_, a) = handle(
            false,
            true,
            &mut c,
            InMsg::Text {
                content: "hello".into(),
                auto_enter: false,
                send_mode: "shadow".into(),
            },
        );
        assert_eq!(a, vec![Action::Text("lo".into())]);
    }

    #[test]
    fn shadow_mismatch_does_not_clear() {
        let mut c = Conn {
            shadow: "abc".into(),
            ..Conn::default()
        };
        let (_, a) = handle(
            false,
            true,
            &mut c,
            InMsg::Text {
                content: "xyz".into(),
                auto_enter: false,
                send_mode: "shadow".into(),
            },
        );
        assert!(a.is_empty());
        assert_eq!(c.shadow, "abc");
    }

    #[test]
    fn submit_pastes_suffix_then_enter() {
        let mut c = Conn {
            shadow: "hi".into(),
            ..Conn::default()
        };
        let (replies, a) = handle(
            false,
            true,
            &mut c,
            InMsg::Text {
                content: "hi there".into(),
                auto_enter: true,
                send_mode: "submit".into(),
            },
        );
        assert_eq!(a, vec![Action::Text(" there".into()), Action::Enter]);
        assert!(matches!(
            replies[0],
            OutMsg::Ack {
                ok: true,
                clear_input: true
            }
        ));
        assert!(c.shadow.is_empty());
    }

    #[test]
    fn pause_drops_input() {
        let mut c = Conn::default();
        let (replies, a) = handle(
            true,
            false,
            &mut c,
            InMsg::Text {
                content: "nope".into(),
                auto_enter: false,
                send_mode: "submit".into(),
            },
        );
        assert!(a.is_empty());
        assert_eq!(
            replies,
            vec![OutMsg::Ack {
                ok: false,
                clear_input: false
            }]
        );
        let (replies, a) = handle(true, false, &mut c, InMsg::Undo);
        assert!(a.is_empty());
        assert!(matches!(replies[0], OutMsg::Ack { ok: false, .. }));
        let (replies, _) = handle(true, false, &mut c, InMsg::Ping);
        assert_eq!(
            replies,
            vec![OutMsg::Pong {
                sync_enabled: false
            }]
        );
    }

    fn pointer_msg(dx: f64, dy: f64, buttons: u8, wheel: i32) -> InMsg {
        InMsg::Pointer {
            dx,
            dy,
            buttons,
            wheel,
        }
    }

    #[test]
    fn pause_releases_only_a_stuck_button() {
        let mut c = Conn::default();
        let (_, a) = handle(false, true, &mut c, pointer_msg(1.5, -2.0, 1, 0));
        assert_eq!(
            a,
            vec![Action::Pointer {
                dx: 1.5,
                dy: -2.0,
                buttons: 1,
                wheel: 0,
            }]
        );
        assert_eq!(c.buttons, 1);

        let (_, a) = handle(true, false, &mut c, pointer_msg(9.0, 9.0, 1, 3));
        assert!(a.is_empty());
        assert_eq!(c.buttons, 1);

        let (_, a) = handle(true, false, &mut c, pointer_msg(9.0, 8.0, 0, 4));
        assert_eq!(
            a,
            vec![Action::Pointer {
                dx: 0.0,
                dy: 0.0,
                buttons: 0,
                wheel: 0,
            }]
        );
        assert_eq!(c.buttons, 0);

        let (_, a) = handle(true, false, &mut c, pointer_msg(1.0, 1.0, 0, 0));
        assert!(a.is_empty());
        assert!(release_stuck_pointer(&mut c).is_none());
    }

    #[test]
    fn release_stuck_pointer_fires_once() {
        let mut c = Conn::default();
        assert!(release_stuck_pointer(&mut c).is_none());
        c.buttons = 1;
        assert_eq!(
            release_stuck_pointer(&mut c),
            Some(Action::Pointer {
                dx: 0.0,
                dy: 0.0,
                buttons: 0,
                wheel: 0,
            })
        );
        assert!(release_stuck_pointer(&mut c).is_none());
    }
}
