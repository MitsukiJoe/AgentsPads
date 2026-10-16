#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod elevation;
mod handle;
mod identity;
mod logutil;
mod net;
mod pairing;
#[cfg(all(windows, feature = "perf-probe"))]
mod perf_probe;
mod protocol;
mod qr;
mod ui;
mod updater;
mod ws;

use std::net::SocketAddr;

#[derive(Debug, PartialEq, Eq)]
enum AddrInUseAction {
    ShowConflict,
    ShowHandoffFailure,
    ExitElevatedFailure,
}

fn addr_in_use_action(relaunched: bool, handoff_failed: bool) -> AddrInUseAction {
    if handoff_failed {
        AddrInUseAction::ShowHandoffFailure
    } else if relaunched {
        AddrInUseAction::ExitElevatedFailure
    } else {
        AddrInUseAction::ShowConflict
    }
}

fn main() -> eframe::Result {
    elevation::exit_if_admin_maintenance();
    if elevation::invalid_elevation_attempt() {
        exit_process(1);
    }
    // Windows：开机启动项由未提权进程在提权重启前同步；提权进程里 apply 什么也不做。
    // macOS 没有提权分支，仍在下面原来的位置调用，相对更新检查的顺序不变。
    if cfg!(windows) {
        autostart::apply();
    }
    // 设置里打开管理员模式时，新的普通进程要等旧进程退出并放开端口，再交接。
    if let Err(reason) = elevation::wait_for_handoff_parent() {
        let ui_result = ui::show_startup_failure(reason);
        if let Some(exit_code) = handoff_failure_exit_code(&ui_result) {
            exit_process(exit_code);
        }
        return ui_result;
    }
    match elevation::relaunch_if_needed() {
        elevation::RelaunchOutcome::ContinueOrdinaryStartup => {}
        elevation::RelaunchOutcome::HandoffStarted => return Ok(()),
        elevation::RelaunchOutcome::ProtectedIdentityStoreFailed => {
            let ui_result = ui::show_startup_failure(
                "管理员身份密钥存储失败。为保护设备身份，AgentsPads 已停止启动；修复受保护存储后请重试。",
            );
            if let Some(exit_code) = handoff_failure_exit_code(&ui_result) {
                exit_process(exit_code);
            }
            return ui_result;
        }
    }
    let post_update = updater::is_post_update_launch();
    if !cfg!(windows) {
        autostart::apply();
    }
    updater::cleanup_stale_updater_script();
    let identity = match identity::load() {
        Ok(identity) => identity,
        Err(err) => {
            // 管理员实例拿不到受保护的密钥时不接收输入，也不回退普通密钥。
            // 失败说明只写 Program Files 的 state\；普通进程这里是空操作。
            logutil::write("identity store failed");
            elevation::note_startup_failure("identity store failed", &err.to_string());
            exit_process(1);
        }
    };
    logutil::clear();
    #[cfg(all(windows, feature = "perf-probe"))]
    let _trace_enabled = perf_probe::initialize();

    let rt = tokio::runtime::Runtime::new().expect("tokio");
    let state = ws::AppState::new(identity);
    let bind = SocketAddr::from(([0, 0, 0, 0], protocol::PORT));
    match rt.block_on(ws::serve_with_retry(
        state.clone(),
        bind,
        post_update || elevation::relaunched(),
    )) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            logutil::write("connection listen already_running");
            elevation::note_startup_failure("listen already in use", "");
            let handoff_failed = {
                #[cfg(windows)]
                {
                    elevation::handoff_failure().is_some()
                }
                #[cfg(not(windows))]
                {
                    false
                }
            };
            match addr_in_use_action(elevation::relaunched(), handoff_failed) {
                AddrInUseAction::ShowConflict => {
                    return show_port_conflict_failure(
                        "9618 端口已被占用，AgentsPads 无法启动。可能已有 AgentsPads 实例在运行，可从系统托盘检查；如果没有，请关闭占用 9618 端口的程序后重试。",
                    );
                }
                AddrInUseAction::ShowHandoffFailure => {
                    return show_port_conflict_failure(
                        "管理员模式未就绪，9618 端口已被占用。请从系统托盘检查是否已有 AgentsPads 实例，或关闭占用端口的程序后重试。",
                    );
                }
                AddrInUseAction::ExitElevatedFailure => exit_process(1),
            }
        }
        Err(_) => {
            logutil::write("connection listen failed");
            elevation::note_startup_failure("listen failed", "");
            #[cfg(windows)]
            if elevation::handoff_failure().is_some() {
                return ui::show_startup_failure("管理员模式未就绪，当前也无法监听 9618 端口。");
            }
            exit_process(1);
        }
    }

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([ui::WINDOW_W, 860.0])
        .with_title("AgentsPads")
        .with_resizable(false);
    #[cfg(target_os = "windows")]
    let viewport = {
        let app_icon =
            eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon_white.png"))
                .expect("valid app icon");
        viewport.with_icon(app_icon)
    };
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let result = eframe::run_native(
        "AgentsPads",
        native_options,
        Box::new(move |cc| Ok(Box::new(ui::PairingApp::new(cc, state)))),
    );
    #[cfg(all(windows, feature = "perf-probe"))]
    if let Some(probe) = perf_probe::active() {
        probe.record("gui_returned", Some("main_window"), None, None, None, None);
    }
    #[cfg(windows)]
    {
        handle::stop_input();
        #[cfg(feature = "perf-probe")]
        if let Some(probe) = perf_probe::active() {
            probe.record(
                "stop_input_done",
                Some("main_shutdown"),
                None,
                None,
                None,
                None,
            );
        }
    }
    if result.is_err() {
        elevation::note_startup_failure("GUI initialization failed", "");
    }
    #[cfg(all(windows, feature = "perf-probe"))]
    if let Some(probe) = perf_probe::active() {
        probe.record(
            "listener_runtime_drop_begin",
            Some("main_shutdown"),
            None,
            None,
            None,
            None,
        );
    }
    drop(rt);
    #[cfg(all(windows, feature = "perf-probe"))]
    if let Some(probe) = perf_probe::active() {
        probe.record(
            "listener_runtime_drop_complete",
            Some("main_shutdown"),
            None,
            None,
            None,
            None,
        );
        probe.record(
            "before_process_exit",
            Some("main_return"),
            None,
            None,
            None,
            None,
        );
        let _ = probe.finalize();
    }
    result
}

fn show_port_conflict_failure(reason: &str) -> eframe::Result {
    ui::show_startup_failure(reason)?;
    exit_process(1)
}

fn exit_process(code: i32) -> ! {
    #[cfg(all(windows, feature = "perf-probe"))]
    if let Some(probe) = perf_probe::active() {
        probe.record(
            "before_process_exit",
            Some("main_exit"),
            None,
            None,
            None,
            None,
        );
        let _ = probe.finalize();
    }
    std::process::exit(code)
}

fn handoff_failure_exit_code<E>(ui_result: &Result<(), E>) -> Option<i32> {
    ui_result.is_ok().then_some(1)
}

#[cfg(test)]
mod addr_in_use_tests {
    use super::{addr_in_use_action, handoff_failure_exit_code, AddrInUseAction};

    #[test]
    fn closing_handoff_failure_window_maps_to_nonzero_exit() {
        assert_eq!(handoff_failure_exit_code(&Ok::<(), &str>(())), Some(1));
    }

    #[test]
    fn handoff_failure_ui_error_stays_on_error_path() {
        assert_eq!(handoff_failure_exit_code(&Err::<(), _>("ui failed")), None);
    }

    #[test]
    fn ordinary_port_conflict_shows_failure() {
        assert_eq!(
            addr_in_use_action(false, false),
            AddrInUseAction::ShowConflict
        );
    }

    #[test]
    fn elevated_port_conflict_returns_failure_to_handoff_parent() {
        assert_eq!(
            addr_in_use_action(true, false),
            AddrInUseAction::ExitElevatedFailure
        );
    }

    #[test]
    fn prior_handoff_failure_keeps_visible_failure_path() {
        assert_eq!(
            addr_in_use_action(false, true),
            AddrInUseAction::ShowHandoffFailure
        );
    }
}
