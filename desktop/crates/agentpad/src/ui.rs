#[cfg(windows)]
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, FontData, FontDefinitions, FontFamily, TextureHandle, TextureOptions, ThemePreference,
    Vec2,
};
use egui_material_icons::icons::{
    ICON_ADD, ICON_CHECK, ICON_CLOSE, ICON_COMPUTER, ICON_CONTENT_COPY, ICON_DARK_MODE, ICON_HELP,
    ICON_HUB, ICON_INFO, ICON_LAN, ICON_LIGHT_MODE, ICON_OPEN_IN_NEW, ICON_REFRESH,
    ICON_SETTINGS_ETHERNET, ICON_SMARTPHONE, ICON_TOOLBAR, ICON_VPN_LOCK, ICON_WIFI,
};
use egui_material_icons::MaterialIcon;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
#[cfg(any(windows, test))]
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

use crate::net::{self, Nic, NicKind};
use crate::protocol::{QrPayload, PORT};
use crate::qr;
use crate::ws::AppState;

pub const WINDOW_W: f32 = 760.0;
const WINDOW_MAX_H: f32 = 860.0;
const MARGIN_X: f32 = 24.0;
const MARGIN_Y: f32 = 20.0;
const CONTENT_W: f32 = WINDOW_W - MARGIN_X * 2.0;
const BAR_H: f32 = 36.0;
const BAR_GAP: f32 = 18.0;
const BODY_H: f32 = 270.0;
const QR_SIDE: f32 = 270.0;
const QR_PAD: f32 = 10.0;
const QR_IMG: f32 = QR_SIDE - QR_PAD * 2.0;
const QR_R: u8 = 14;
const COL_GAP: f32 = 20.0;
const RIGHT_W: f32 = CONTENT_W - QR_SIDE - COL_GAP;
const CARD_PAD: f32 = 16.0;
const CARD_R: u8 = 12;
const PANEL_GAP: f32 = 16.0;
const TILE_H: f32 = 62.0;
const TILE_GAP: f32 = 8.0;
const CODE_W: f32 = 104.0;
const ADDR_W: f32 = RIGHT_W - CODE_W - TILE_GAP;
const COL_AFTER_TILE: f32 = 12.0;
const NIC_TITLE_H: f32 = 16.0;
const COL_AFTER_TITLE: f32 = 8.0;
const NIC_H: f32 = 48.0;
const NIC_GAP: f32 = 7.0;
const NIC_LIST_PAD_X: f32 = 6.0;
const NIC_LIST_PAD_R: f32 = 4.0;
const NIC_LIST_PAD_Y: f32 = 6.0;
const NIC_SCROLL_GAP: f32 = 4.0;
const NIC_SCROLL_W: f32 = 4.0;
const NIC_LIST_STROKE: f32 = 1.0;
const SEG_W: f32 = 34.0;
const THEME_PAD: f32 = 4.0;
const THEME_GAP: f32 = 2.0;
const THEME_W: f32 = THEME_PAD * 2.0 + SEG_W * 3.0 + THEME_GAP * 2.0;
const GUIDE_W: f32 = 460.0;
const GUIDE_H: f32 = 282.0;
const GUIDE_ANIM: f32 = 0.30;
const EMPTY_ANIM: f32 = 0.25;
/// Long help reads better wrapped; kept under the window so the tail is not clipped.
const TOOLTIP_W: f32 = 360.0;
#[cfg(windows)]
const ADMIN_HELP: &str = "让 AgentsPads 能控制任务管理器等需要管理员权限的窗口。开启时需要确认一次，程序会被复制到只有管理员才能修改的系统文件夹，之后都从那里运行，其他软件没法偷偷替换它。不用时建议关闭。";
const FOOTER: &str = if cfg!(windows) {
    "关闭窗口后仍在系统托盘运行 · 从托盘菜单退出"
} else {
    "关闭窗口后仍在菜单栏运行 · 从菜单栏图标退出"
};
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const WATCH_INTERVAL: Duration = Duration::from_secs(1);
const COPY_FEEDBACK: Duration = Duration::from_secs(2);
const RESET_ARM: Duration = Duration::from_secs(4);

/// Everything outside egui input that the window shows. A background thread
/// samples it and repaints only on change: on macOS a window that keeps
/// presenting frames pins ~400 MB of GPU driver memory, released only when idle.
#[derive(Clone, PartialEq)]
struct Watched {
    nics: Vec<Nic>,
    ax_ok: bool,
    code: Option<String>,
    paused: bool,
    update: crate::updater::UpdateStatus,
}

impl Watched {
    fn sample(state: &AppState, updater: &crate::updater::Updater) -> Self {
        Self {
            nics: net::list_nics(),
            ax_ok: agentpad_input::accessibility_trusted(),
            code: state.pairing.lock().unwrap().code().map(str::to_string),
            paused: state.paused.load(std::sync::atomic::Ordering::SeqCst),
            update: updater.status.lock().unwrap().clone(),
        }
    }
}

// ponytail: 1 s polling thread; switch to OS change notifications if the lag matters.
fn spawn_watcher(
    ctx: egui::Context,
    state: Arc<AppState>,
    updater: Arc<crate::updater::Updater>,
    watched: Arc<Mutex<Watched>>,
) {
    std::thread::spawn(move || {
        let mut last_update_check = Instant::now();
        loop {
            std::thread::sleep(WATCH_INTERVAL);
            if last_update_check.elapsed() >= UPDATE_CHECK_INTERVAL {
                updater.check_for_updates();
                last_update_check = Instant::now();
            }
            let now = Watched::sample(&state, &updater);
            let mut w = watched.lock().unwrap();
            if *w != now {
                *w = now;
                ctx.request_repaint();
            }
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Pair,
    Settings,
    About,
}

fn initial_page() -> Page {
    #[cfg(windows)]
    if crate::elevation::handoff_failure().is_some() {
        return Page::Settings;
    }
    Page::Pair
}

pub fn show_startup_failure(reason: &str) -> eframe::Result {
    struct StartupFailure(String);
    impl eframe::App for StartupFailure {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let panel_fill = ui.visuals().panel_fill;
            let text_color = ui.visuals().text_color();
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(panel_fill)
                        .inner_margin(egui::Margin::symmetric(24, 18)),
                )
                .show(ui, |ui| {
                    ui.heading(egui::RichText::new("AgentsPads 启动失败").color(text_color));
                    ui.add_space(12.0);
                    ui.colored_label(text_color, &self.0);
                    ui.add_space(12.0);
                    ui.colored_label(text_color, "当前未接收输入。解决问题后重新打开应用。");
                    if ui.button("关闭").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
        }

        fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
            visuals.panel_fill.to_normalized_gamma_f32()
        }
    }
    let reason = reason.to_string();
    eframe::run_native(
        "AgentsPads",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([520.0, 220.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            Ok(Box::new(StartupFailure(reason)))
        }),
    )
}

pub struct PairingApp {
    state: Arc<AppState>,
    nics: Vec<Nic>,
    selected_ip: String,
    watched: Arc<Mutex<Watched>>,
    menu_rx: std::sync::mpsc::Receiver<MenuEvent>,
    #[cfg(windows)]
    tray_rx: std::sync::mpsc::Receiver<TrayIconEvent>,
    #[cfg(windows)]
    hwnd: Arc<AtomicIsize>,
    #[cfg(windows)]
    ready_published: bool,
    qr_tex: Option<(String, u32, TextureHandle)>,
    tray: Option<TrayIcon>,
    tray_dark: bool,
    #[cfg(target_os = "windows")]
    window_icon_dark: Option<bool>,
    mi_show: MenuItem,
    mi_pause: CheckMenuItem,
    mi_logs: MenuItem,
    mi_quit: MenuItem,
    theme: ThemePreference,
    page: Page,
    guide_open: bool,
    /// First `animate_bool` call snaps to the target, so seed closed once then ease open.
    guide_seeded: bool,
    info_rect: egui::Rect,
    app_icon: Option<(bool, TextureHandle)>,
    quit: bool,
    hidden: bool,
    ax_ok: bool,
    copied_at: Option<Instant>,
    id_copied_at: Option<Instant>,
    reset_armed_at: Option<Instant>,
    sharing_enabled: bool,
    window_h: f32,
    #[cfg(windows)]
    run_as_admin: bool,
    /// Pending admin-mode switch (target state) awaiting the user's confirmation.
    #[cfg(windows)]
    admin_confirm: Option<bool>,
    autostart_enabled: bool,
    updater: Arc<crate::updater::Updater>,
}

fn exit_with_probe(code: i32, _source: &'static str) -> ! {
    #[cfg(all(windows, feature = "perf-probe"))]
    if let Some(probe) = crate::perf_probe::active() {
        probe.record("before_process_exit", Some(_source), None, None, None, None);
        let _ = probe.finalize();
    }
    std::process::exit(code)
}

impl PairingApp {
    pub fn new(cc: &eframe::CreationContext<'_>, state: Arc<AppState>) -> Self {
        install_fonts(&cc.egui_ctx);
        let theme = theme_from_str(&crate::identity::load_theme());
        cc.egui_ctx.set_theme(theme);
        apply_app_style(&cc.egui_ctx);
        let mi_show = MenuItem::new("显示配对码", true, None);
        let mi_pause = CheckMenuItem::new("暂停接收", true, false, None);
        let mi_logs = MenuItem::new("打开日志", true, None);
        let mi_quit = MenuItem::new("退出", true, None);
        let menu = Menu::new();
        let _ = menu.append_items(&[
            &mi_show,
            &mi_pause,
            &mi_logs,
            &PredefinedMenuItem::separator(),
            &mi_quit,
        ]);
        let tray_dark = match theme {
            ThemePreference::Dark => true,
            ThemePreference::Light => false,
            ThemePreference::System => cc.egui_ctx.theme() == egui::Theme::Dark,
        };
        let builder = TrayIconBuilder::new()
            .with_tooltip("AgentsPads")
            .with_menu(Box::new(menu))
            .with_icon(tray_icon(tray_dark));
        // Default is a menu on left click. Right click still opens it
        // (tray-icon shows the menu on WM_RBUTTONDOWN).
        #[cfg(windows)]
        let builder = builder.with_menu_on_left_click(false);
        let tray = match builder.build() {
            Ok(t) => Some(t),
            Err(_e) => {
                crate::logutil::write("tray create failed");
                None
            }
        };
        #[cfg(windows)]
        let hwnd = Arc::new(AtomicIsize::new(0));
        #[cfg(windows)]
        let show_id = mi_show.id().clone();
        #[cfg(windows)]
        let quit_id = mi_quit.id().clone();
        let (menu_tx, menu_rx) = std::sync::mpsc::channel();
        let wake = cc.egui_ctx.clone();
        #[cfg(windows)]
        let hwnd_menu = Arc::clone(&hwnd);
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            #[cfg(windows)]
            let is_quit = ev.id == quit_id;
            #[cfg(windows)]
            if is_quit {
                #[cfg(feature = "perf-probe")]
                if let Some(probe) = crate::perf_probe::active() {
                    probe.record(
                        "tray_exit_callback",
                        Some("menu_event_handler"),
                        None,
                        None,
                        None,
                        None,
                    );
                }
            }
            #[cfg(windows)]
            if ev.id == show_id {
                reveal_pairing_window(&hwnd_menu);
            }
            let send_failed = menu_tx.send(ev).is_err();
            #[cfg(windows)]
            if is_quit && send_failed {
                #[cfg(feature = "perf-probe")]
                if let Some(probe) = crate::perf_probe::active() {
                    probe.record(
                        "tray_exit_callback_send_failed",
                        Some("menu_channel"),
                        Some("receiver_closed"),
                        None,
                        None,
                        None,
                    );
                }
            }
            wake.request_repaint();
        }));
        #[cfg(windows)]
        let tray_rx = {
            let (tray_tx, tray_rx) = std::sync::mpsc::channel();
            let wake = cc.egui_ctx.clone();
            let hwnd_click = Arc::clone(&hwnd);
            TrayIconEvent::set_event_handler(Some(move |ev| {
                // Enter/move/leave fire for the whole hover. Forwarding them
                // would repaint continuously.
                if !tray_click_shows_window(&ev) {
                    return;
                }
                reveal_pairing_window(&hwnd_click);
                let _ = tray_tx.send(ev);
                wake.request_repaint();
            }));
            tray_rx
        };
        let updater = Arc::new(crate::updater::Updater::new());
        updater.check_for_updates();
        let first = Watched::sample(&state, &updater);
        let nics = first.nics.clone();
        let selected_ip = net::default_ip(&nics).unwrap_or_default();
        let ax_ok = first.ax_ok;
        let watched = Arc::new(Mutex::new(first));
        spawn_watcher(
            cc.egui_ctx.clone(),
            state.clone(),
            updater.clone(),
            watched.clone(),
        );

        Self {
            state,
            nics,
            selected_ip,
            watched,
            menu_rx,
            #[cfg(windows)]
            tray_rx,
            #[cfg(windows)]
            hwnd,
            #[cfg(windows)]
            ready_published: false,
            qr_tex: None,
            tray,
            tray_dark,
            #[cfg(target_os = "windows")]
            window_icon_dark: None,
            mi_show,
            mi_pause,
            mi_logs,
            mi_quit,
            theme,
            page: initial_page(),
            guide_open: initial_page() == Page::Pair && !guide_seen(),
            guide_seeded: false,
            info_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::splat(36.0)),
            app_icon: None,
            quit: false,
            hidden: false,
            ax_ok,
            copied_at: None,
            id_copied_at: None,
            reset_armed_at: None,
            sharing_enabled: false,
            window_h: 0.0,
            #[cfg(windows)]
            run_as_admin: crate::elevation::enabled(),
            #[cfg(windows)]
            admin_confirm: None,
            autostart_enabled: crate::autostart::enabled(),
            updater,
        }
    }

    fn sync_desktop_icons(&mut self, _ctx: &egui::Context, _frame: &eframe::Frame, dark: bool) {
        if dark != self.tray_dark {
            if let Some(tray) = &self.tray {
                if let Err(_e) = tray.set_icon(Some(tray_icon(dark))) {
                    crate::logutil::write("tray refresh failed");
                } else {
                    self.tray_dark = dark;
                }
            } else {
                self.tray_dark = dark;
            }
        }

        #[cfg(target_os = "windows")]
        if self.window_icon_dark != Some(dark) {
            let icon = tray_icon_data(dark);
            _ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(icon.clone()))));

            if let Some(window) = _frame.winit_window() {
                use winit::platform::windows::WindowExtWindows;
                let taskbar_icon =
                    winit::window::Icon::from_rgba(icon.rgba, icon.width, icon.height)
                        .expect("valid taskbar icon");
                window.set_taskbar_icon(Some(taskbar_icon));
            }
            self.window_icon_dark = Some(dark);
        }
    }

    fn apply_nics(&mut self, nics: Vec<Nic>) {
        if !nics.iter().any(|n| n.ip == self.selected_ip) {
            self.selected_ip = net::default_ip(&nics).unwrap_or_default();
        }
        self.nics = nics;
    }

    fn payload(&self) -> QrPayload {
        // Phone stores only `ip` on scan; other NICs arrive via `connected.ips`.
        // Leaving them out keeps the QR sparse enough to scan quickly.
        QrPayload::new(
            self.state.identity.device_id.clone(),
            self.state.identity.name.clone(),
            agentpad_input::os().to_string(),
            self.state.secret(),
            self.selected_ip.clone(),
            Vec::new(),
        )
    }

    #[cfg(windows)]
    fn capture_hwnd(&mut self, frame: &eframe::Frame) {
        let Some(window) = frame.winit_window() else {
            return;
        };
        use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        let Ok(handle) = window.window_handle() else {
            return;
        };
        if let RawWindowHandle::Win32(win) = handle.as_raw() {
            self.hwnd.store(win.hwnd.get(), Ordering::Release);
            if !self.ready_published
                && crate::elevation::relaunched()
                && crate::elevation::is_elevated()
            {
                if crate::elevation::publish_ready(win.hwnd.get()).is_err() {
                    crate::elevation::note_startup_failure("ready verification failed", "");
                    crate::handle::stop_input();
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = crate::perf_probe::active() {
                        probe.record(
                            "stop_input_done",
                            Some("ready_publish_failure"),
                            None,
                            None,
                            None,
                            None,
                        );
                    }
                    exit_with_probe(1, "ready_publish_failure");
                }
                self.ready_published = true;
            }
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        reveal_pairing_window(&self.hwnd);
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        {
            let mut show = false;
            while self.tray_rx.try_recv().is_ok() {
                show = true;
            }
            if show {
                self.show_window(ctx);
            }
        }
        while let Ok(ev) = self.menu_rx.try_recv() {
            if ev.id == self.mi_show.id() {
                self.show_window(ctx);
            } else if ev.id == self.mi_pause.id() {
                let next = !self.state.paused.load(std::sync::atomic::Ordering::SeqCst);
                self.state.set_paused(next);
                self.mi_pause.set_checked(next);
            } else if ev.id == self.mi_logs.id() {
                crate::logutil::open_dir();
            } else if ev.id == self.mi_quit.id() {
                #[cfg(all(windows, feature = "perf-probe"))]
                if let Some(probe) = crate::perf_probe::active() {
                    probe.record("poll_tray_exit", Some("poll_tray"), None, None, None, None);
                }
                self.quit = true;
                #[cfg(windows)]
                {
                    crate::handle::stop_input();
                    #[cfg(feature = "perf-probe")]
                    if let Some(probe) = crate::perf_probe::active() {
                        probe.record(
                            "stop_input_done",
                            Some("tray_quit_handler"),
                            None,
                            None,
                            None,
                            None,
                        );
                    }
                }
                crate::logutil::write("quit from tray");
                ctx.request_repaint();
                // Hidden pairing window: Viewport Close is often ignored once on
                // Windows (event loop keeps running until a second quit). Exit
                // directly when already in tray-only mode.
                if self.hidden {
                    exit_with_probe(0, "hidden_tray_quit");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn apply_window_cmds(&mut self, ctx: &egui::Context) {
        // read viewport without holding the lock across send_viewport_cmd
        let (close, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (v.close_requested(), v.minimized == Some(true))
        });
        if close && !self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
        if minimized && !self.quit && !self.hidden {
            self.hide_window(ctx);
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.poll_tray(ctx);
        self.apply_window_cmds(ctx);
        self.state
            .pairing
            .lock()
            .unwrap()
            .set_window_open(!self.hidden);
        let (nics, ax) = {
            let w = self.watched.lock().unwrap();
            ((w.nics != self.nics).then(|| w.nics.clone()), w.ax_ok)
        };
        if let Some(nics) = nics {
            self.apply_nics(nics);
        }
        ctx.set_theme(self.theme);
        apply_app_style(ctx);
        self.poll_ax(ax);
    }

    fn poll_ax(&mut self, ax: bool) {
        if ax != self.ax_ok {
            crate::logutil::write(if ax {
                "permission check granted"
            } else {
                "permission check denied"
            });
        }
        self.ax_ok = ax;
    }
}

impl eframe::App for PairingApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Publish the HWND before poll_tray. A hidden window may skip `ui`,
        // and show_window in this same turn is what syncs egui after the click.
        #[cfg(windows)]
        {
            self.capture_hwnd(frame);
            if self.hidden
                && frame
                    .winit_window()
                    .is_some_and(|window| window.is_visible() == Some(true))
            {
                self.show_window(ctx);
            }
        }
        #[cfg(not(windows))]
        let _ = frame;
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !self.sharing_enabled {
            if let Some(window) = frame.winit_window() {
                window.set_content_protected(false);
                self.sharing_enabled = true;
            }
        }
        let ctx = ui.ctx().clone();
        let dark = ui.visuals().dark_mode;
        self.sync_desktop_icons(&ctx, frame, dark);
        let p = Pal::new(dark);
        let notice = permission_notice(crate::autostart::running_from_app_bundle(), self.ax_ok);

        let mut content_h = 0.0;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.page))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        content_h = egui::Frame::new()
                            .inner_margin(egui::Margin::symmetric(MARGIN_X as i8, MARGIN_Y as i8))
                            .show(ui, |ui| {
                                ui.set_width(CONTENT_W);
                                ui.spacing_mut().item_spacing = Vec2::ZERO;
                                self.header(ui, &ctx, p);
                                ui.add_space(BAR_GAP);
                                if self.page == Page::Pair {
                                    if let Some(notice) = notice {
                                        self.notice_card(ui, notice, p);
                                        ui.add_space(12.0);
                                    }
                                }
                                let (body, _) = ui.allocate_exact_size(
                                    Vec2::new(CONTENT_W, BODY_H),
                                    egui::Sense::hover(),
                                );
                                match self.page {
                                    Page::Pair => self.pair_page(ui, &ctx, body, p),
                                    Page::Settings => self.settings_page(ui, body, p),
                                    Page::About => self.about_page(ui, &ctx, body, p),
                                }
                            })
                            .response
                            .rect
                            .height();
                    });
            });
        let monitor = ctx.input(|i| i.viewport().monitor_size.map(|m| m.y));
        let h = fitted_height(content_h, monitor);
        if (h - self.window_h).abs() >= 1.0 {
            self.window_h = h;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(WINDOW_W, h)));
        }
        self.guide_overlay(&ctx, p);
        #[cfg(windows)]
        self.admin_confirm_overlay(&ctx, p);
    }
}

impl PairingApp {
    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: Pal) {
        let (bar, _) = ui.allocate_exact_size(Vec2::new(CONTENT_W, BAR_H), egui::Sense::hover());
        let info = egui::Rect::from_min_size(bar.min, Vec2::splat(BAR_H));
        self.info_rect = info;
        let info_resp = ui
            .interact(info, ui.id().with("guide-btn"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("查看连接引导");
        ui.painter().rect(
            info,
            10,
            if info_resp.hovered() { p.hover } else { p.card },
            egui::Stroke::new(1.0, p.line),
            egui::StrokeKind::Inside,
        );
        paint_icon(
            ui,
            info.center(),
            ICON_INFO,
            18.0,
            if info_resp.hovered() { p.text } else { p.muted },
        );
        if info_resp.clicked() {
            if self.guide_open {
                self.close_guide();
            } else {
                self.open_guide();
            }
        }

        let tabs = self.tab_bar(ui, bar, p);
        self.theme_bar(ui, ctx, bar, p);

        let status = egui::Rect::from_min_max(
            egui::pos2(info.right() + 10.0, bar.top()),
            egui::pos2(tabs.left() - 8.0, bar.bottom()),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(status)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, p.ok);
                ui.label(small(
                    format!("监听 {PORT} · {}", crate::updater::version_label()),
                    p.muted,
                ));
                self.header_update(ui, p);
            },
        );
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui, bar: egui::Rect, p: Pal) -> egui::Rect {
        let labels = ["配对", "设置", "关于"];
        let pages = [Page::Pair, Page::Settings, Page::About];
        let text_w = labels.iter().fold(0.0_f32, |w, label| {
            w.max(
                ui.painter()
                    .layout_no_wrap((*label).into(), egui::FontId::proportional(13.0), p.text)
                    .size()
                    .x,
            )
        });
        let item_w = text_w + 32.0;
        let outer_w = THEME_PAD * 2.0 + item_w * 3.0 + THEME_GAP * 2.0;
        let outer = egui::Rect::from_center_size(bar.center(), Vec2::new(outer_w, BAR_H));
        ui.painter().rect(
            outer,
            10,
            p.card,
            egui::Stroke::new(1.0, p.line),
            egui::StrokeKind::Inside,
        );
        for (i, (label, page)) in labels.iter().zip(pages).enumerate() {
            let r = egui::Rect::from_min_size(
                egui::pos2(
                    outer.left() + THEME_PAD + (item_w + THEME_GAP) * i as f32,
                    outer.top() + THEME_PAD,
                ),
                Vec2::new(item_w, BAR_H - THEME_PAD * 2.0),
            );
            let resp = ui
                .interact(r, ui.id().with(("page", i)), egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let selected = self.page == page;
            if selected || resp.hovered() {
                ui.painter().rect_filled(r, 7, p.hover);
            }
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                *label,
                egui::FontId::proportional(13.0),
                if selected || resp.hovered() {
                    p.text
                } else {
                    p.muted
                },
            );
            if resp.clicked() {
                self.page = page;
            }
        }
        outer
    }

    fn theme_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, bar: egui::Rect, p: Pal) {
        let seg = egui::Rect::from_min_size(
            egui::pos2(bar.right() - THEME_W, bar.top()),
            Vec2::new(THEME_W, BAR_H),
        );
        ui.painter().rect(
            seg,
            10,
            p.card,
            egui::Stroke::new(1.0, p.line),
            egui::StrokeKind::Inside,
        );
        for (i, this) in [
            ThemePreference::System,
            ThemePreference::Light,
            ThemePreference::Dark,
        ]
        .into_iter()
        .enumerate()
        {
            let r = egui::Rect::from_min_size(
                egui::pos2(
                    seg.left() + THEME_PAD + (SEG_W + THEME_GAP) * i as f32,
                    seg.top() + THEME_PAD,
                ),
                Vec2::new(SEG_W, BAR_H - THEME_PAD * 2.0),
            );
            let (icon, tip) = theme_icon_spec(this);
            let resp = ui
                .interact(r, ui.id().with(("theme", i)), egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(tip);
            let selected = self.theme == this;
            let fill = if selected {
                p.accent
            } else if resp.hovered() {
                p.hover
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect_filled(r, 7, fill);
            let color = if selected { p.on_accent } else { p.muted };
            paint_icon(ui, r.center(), icon, 16.0, color);
            if resp.clicked() && !selected {
                self.theme = this;
                crate::identity::save_theme(theme_to_str(this));
                ctx.set_theme(this);
                apply_app_style(ctx);
            }
        }
    }

    /// Pairing header only keeps the states that need a click to download.
    /// Checking / up to date / check failed live on the About page.
    fn header_update(&mut self, ui: &mut egui::Ui, p: Pal) {
        use crate::updater::UpdateStatus;
        let status = self.updater.status.lock().unwrap().clone();
        match status {
            UpdateStatus::Available(info) => {
                if link(ui, &format!("更新到 v{}", info.version), p.ok)
                    .on_hover_text("确认下载并安装此版本")
                    .clicked()
                {
                    self.updater.start_update(&info);
                }
            }
            UpdateStatus::Updating(msg) => {
                ui.label(small(msg, p.accent));
            }
            UpdateStatus::UpdateFailed { info, message }
                if link(ui, "更新失败 · 重试", p.warn)
                    .on_hover_text(&message)
                    .clicked() =>
            {
                self.updater.start_update(&info);
            }
            _ => {}
        }
    }

    fn notice_card(&mut self, ui: &mut egui::Ui, notice: PermissionNotice, p: Pal) {
        let inner_w = CONTENT_W - CARD_PAD * 2.0 - 2.0;
        let (title, body) = match notice {
            PermissionNotice::DevelopmentLaunch => (
                "请从 AgentsPads.app 启动",
                "当前是开发启动方式。请关闭此实例后打开打包应用，不要给启动器授权。",
            ),
            PermissionNotice::NeedsAccess => (
                "需要辅助功能权限",
                "用于从手机发送按键并控制指针。若更新后列表里已有 AgentPad 或 AgentsPads 但授权无效，请先删除旧项，再点击下方按钮为当前版本重新授权。",
            ),
        };
        egui::Frame::new()
            .fill(p.warn.gamma_multiply(0.12))
            .stroke(egui::Stroke::new(1.0, p.warn.gamma_multiply(0.6)))
            .corner_radius(16)
            .inner_margin(CARD_PAD)
            .show(ui, |ui| {
                ui.set_width(inner_w);
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui_material_icons::icons::ICON_WARNING
                            .rich_text()
                            .size(16.0)
                            .color(p.warn),
                    );
                    ui.label(egui::RichText::new(title).size(14.0).color(p.text));
                });
                ui.label(egui::RichText::new(body).size(12.5).color(p.muted));
                if notice == PermissionNotice::NeedsAccess {
                    ui.add_space(2.0);
                    let (row, _) =
                        ui.allocate_exact_size(Vec2::new(inner_w, 32.0), egui::Sense::hover());
                    let a = egui::Rect::from_min_size(row.min, Vec2::new(96.0, 32.0));
                    let b = egui::Rect::from_min_size(
                        egui::pos2(a.right() + 8.0, row.top()),
                        Vec2::new(104.0, 32.0),
                    );
                    if pill_button(
                        ui,
                        a,
                        "ax-request",
                        "请求授权",
                        p.on_accent,
                        Some(p.accent),
                        p,
                    )
                    .clicked()
                    {
                        agentpad_input::prompt_accessibility();
                        self.ax_ok = agentpad_input::accessibility_trusted();
                    }
                    if pill_button(ui, b, "ax-settings", "打开系统设置", p.text, None, p).clicked()
                    {
                        agentpad_input::open_accessibility_settings();
                    }
                }
            });
    }

    fn pair_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, body: egui::Rect, p: Pal) {
        let qr = egui::Rect::from_min_size(body.min, Vec2::splat(QR_SIDE));
        let col = egui::Rect::from_min_size(
            egui::pos2(qr.right() + COL_GAP, body.top()),
            Vec2::new(RIGHT_W, QR_SIDE),
        );
        self.paint_qr(ui, ctx, qr, p);
        self.paint_tiles(ui, ctx, col, p);
        let title = egui::Rect::from_min_size(
            egui::pos2(col.left(), col.top() + TILE_H + COL_AFTER_TILE),
            Vec2::new(RIGHT_W, NIC_TITLE_H),
        );
        self.paint_nic_title(ui, title, p);
        let list = egui::Rect::from_min_size(
            egui::pos2(col.left(), title.bottom() + COL_AFTER_TITLE),
            Vec2::new(RIGHT_W, nic_list_height()),
        );
        self.paint_nic_list(ui, ctx, list, p);
    }

    fn paint_qr(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, card: egui::Rect, p: Pal) {
        let px = qr::raster_px(ctx.pixels_per_point());
        let payload = serde_json::to_string(&self.payload()).unwrap_or_default();
        let stale = self
            .qr_tex
            .as_ref()
            .map(|(saved, s, _)| saved != &payload || *s != px)
            .unwrap_or(true);
        if stale {
            if let Ok(img) = qr::color_image(&payload, px) {
                let tex = ctx.load_texture("qr", img, TextureOptions::NEAREST);
                self.qr_tex = Some((payload, px, tex));
            }
        }
        ui.painter().rect(
            card,
            QR_R,
            egui::Color32::WHITE,
            egui::Stroke::new(1.0, p.line),
            egui::StrokeKind::Inside,
        );
        let img = egui::Rect::from_center_size(card.center(), Vec2::splat(QR_IMG));
        if let Some((_, _, tex)) = &self.qr_tex {
            ui.put(img, egui::Image::new((tex.id(), Vec2::splat(QR_IMG))));
        }
    }

    fn paint_tiles(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, col: egui::Rect, p: Pal) {
        let row = egui::Rect::from_min_size(col.min, Vec2::new(col.width(), TILE_H));
        let addr_rect = egui::Rect::from_min_size(row.min, Vec2::new(ADDR_W, TILE_H));
        let code_rect = egui::Rect::from_min_size(
            egui::pos2(addr_rect.right() + TILE_GAP, row.top()),
            Vec2::new(CODE_W, TILE_H),
        );
        let addr = format!("{}:{PORT}", self.selected_ip);
        let copied = copy_label(self.copied_at, Instant::now()) == "已复制";
        repaint_when_expired(ctx, self.copied_at, COPY_FEEDBACK);
        let (label, icon) = if copied {
            ("已复制", ICON_CHECK)
        } else {
            ("地址", ICON_CONTENT_COPY)
        };
        if tile(ui, addr_rect, "addr", label, &addr, p.text, icon, p)
            .on_hover_text("点击复制")
            .clicked()
        {
            ctx.copy_text(addr);
            self.copied_at = Some(Instant::now());
            ctx.request_repaint();
        }

        let code = self
            .state
            .pairing
            .lock()
            .unwrap()
            .code()
            .map(str::to_string);
        let (value, color, tip) = match &code {
            Some(code) => (
                code.as_str(),
                p.text,
                "仅手动输入地址时需要；扫码无需配对码。窗口隐藏后失效，用过即换新。点击换一个。",
            ),
            None => ("已锁定", p.warn, "输错次数过多已锁定，点击换一个新配对码。"),
        };
        if tile(
            ui,
            code_rect,
            "code",
            "配对码",
            value,
            color,
            ICON_REFRESH,
            p,
        )
        .on_hover_text(tip)
        .clicked()
        {
            self.state.pairing.lock().unwrap().renew();
        }
    }

    fn paint_nic_title(&self, ui: &mut egui::Ui, row: egui::Rect, p: Pal) {
        let kind =
            ui.painter()
                .layout_no_wrap("网卡".into(), egui::FontId::proportional(13.0), p.text);
        let hint = ui.painter().layout_no_wrap(
            "二维码与地址使用选中的网卡".into(),
            egui::FontId::proportional(12.0),
            p.faint,
        );
        let kind_y = row.center().y - kind.size().y / 2.0;
        ui.painter()
            .galley(egui::pos2(row.left(), kind_y), kind.clone(), p.text);
        ui.painter().galley(
            egui::pos2(
                row.left() + kind.size().x + 8.0,
                row.center().y - hint.size().y / 2.0,
            ),
            hint,
            p.faint,
        );
        ui.painter().text(
            row.right_center(),
            egui::Align2::RIGHT_CENTER,
            nic_counter(&self.nics, &self.selected_ip),
            egui::FontId::proportional(12.0),
            p.muted,
        );
    }

    fn paint_nic_list(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        panel: egui::Rect,
        p: Pal,
    ) {
        ui.painter().rect(
            panel,
            CARD_R,
            p.card,
            egui::Stroke::new(NIC_LIST_STROKE, p.line),
            egui::StrokeKind::Inside,
        );
        let row_w = nic_row_w(panel.width());
        let track = egui::Rect::from_min_size(
            egui::pos2(
                panel.left() + NIC_LIST_STROKE + NIC_LIST_PAD_X,
                panel.top() + NIC_LIST_STROKE + NIC_LIST_PAD_Y,
            ),
            Vec2::new(row_w, nic_track_h()),
        );
        let n = self.nics.len();
        let mut pick = None;
        if n > 3 {
            let scroll = egui::Rect::from_min_size(
                track.min,
                Vec2::new(row_w + NIC_SCROLL_GAP + NIC_SCROLL_W, track.height()),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(scroll), |ui| {
                ui.style_mut().spacing.item_spacing = Vec2::ZERO;
                let bar = &mut ui.style_mut().spacing.scroll;
                bar.floating = false;
                bar.bar_width = NIC_SCROLL_W;
                bar.bar_inner_margin = NIC_SCROLL_GAP;
                bar.bar_outer_margin = 0.0;
                bar.handle_min_length = 18.0;
                egui::ScrollArea::vertical()
                    .id_salt("nics")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        ui.set_width(row_w);
                        for (i, nic) in self.nics.iter().enumerate() {
                            if i > 0 {
                                ui.add_space(NIC_GAP);
                            }
                            let (r, _) = ui
                                .allocate_exact_size(Vec2::new(row_w, NIC_H), egui::Sense::hover());
                            if paint_nic_row(ui, r, i, nic, nic.ip == self.selected_ip, p) {
                                pick = Some(nic.ip.clone());
                            }
                        }
                    });
            });
        } else {
            let mut y = track.top();
            for (i, nic) in self.nics.iter().enumerate() {
                let r =
                    egui::Rect::from_min_size(egui::pos2(track.left(), y), Vec2::new(row_w, NIC_H));
                if paint_nic_row(ui, r, i, nic, nic.ip == self.selected_ip, p) {
                    pick = Some(nic.ip.clone());
                }
                y += NIC_H;
                if i + 1 < n || empty_slot_h(n) > 0.0 {
                    y += NIC_GAP;
                }
            }
            let slot_h = empty_slot_h(n);
            if slot_h > 0.0 {
                let slot = egui::Rect::from_min_size(
                    egui::pos2(track.left(), y),
                    Vec2::new(row_w, slot_h),
                );
                paint_empty_slot(ui, ctx, slot, p);
            }
        }
        if let Some(ip) = pick {
            self.selected_ip = ip;
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, body: egui::Rect, p: Pal) {
        let card_w = (body.width() - PANEL_GAP) / 2.0;
        let left = egui::Rect::from_min_size(body.min, Vec2::new(card_w, BODY_H));
        let right = egui::Rect::from_min_size(
            egui::pos2(left.right() + PANEL_GAP, body.top()),
            Vec2::new(body.right() - left.right() - PANEL_GAP, BODY_H),
        );
        paint_panel(ui, left, p);
        paint_panel(ui, right, p);
        self.general_card(ui, left, p);
        self.diag_card(ui, right, p);
    }

    fn general_card(&mut self, ui: &mut egui::Ui, card: egui::Rect, p: Pal) {
        let inner = card.shrink(17.0);
        let footer = egui::Rect::from_min_max(
            egui::pos2(inner.left(), inner.bottom() - 14.0),
            inner.right_bottom(),
        );
        paint_icon(
            ui,
            egui::pos2(footer.left() + 7.0, footer.center().y),
            ICON_TOOLBAR,
            14.0,
            p.faint,
        );
        ui.painter().text(
            egui::pos2(footer.left() + 20.0, footer.center().y),
            egui::Align2::LEFT_CENTER,
            FOOTER,
            egui::FontId::proportional(12.0),
            p.faint,
        );
        let body =
            egui::Rect::from_min_max(inner.min, egui::pos2(inner.right(), footer.top() - 14.0));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                kicker(ui, "常规", p);
                ui.add_space(14.0);
                if crate::autostart::available() {
                    let mut on = self.autostart_enabled;
                    if switch_block(
                        ui,
                        "开机启动",
                        "登录系统后在托盘后台运行",
                        None,
                        "autostart",
                        &mut on,
                        p,
                    ) {
                        match crate::autostart::set_enabled(on) {
                            Ok(()) => self.autostart_enabled = on,
                            Err(_) => crate::logutil::write("autostart setting failed"),
                        }
                    }
                }
                #[cfg(windows)]
                {
                    ui.add_space(14.0);
                    hline(ui, p);
                    ui.add_space(14.0);
                    let mut on = self.run_as_admin;
                    if switch_block(
                        ui,
                        "以管理员模式启动",
                        if crate::elevation::is_elevated() {
                            "当前已提权，可控制管理员窗口"
                        } else {
                            "当前普通权限，无法控制管理员窗口"
                        },
                        Some(ADMIN_HELP),
                        "admin",
                        &mut on,
                        p,
                    ) {
                        // Switching changes the pairing key, so ask first.
                        self.admin_confirm = Some(on);
                    }
                    admin_handoff_warning(ui, p);
                }
            },
        );
    }

    fn diag_card(&mut self, ui: &mut egui::Ui, card: egui::Rect, p: Pal) {
        let inner = card.shrink(17.0);
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                kicker(ui, "诊断与安全", p);
                ui.add_space(14.0);
                let mut on = crate::logutil::enabled();
                if switch_block(
                    ui,
                    "诊断日志",
                    "仅本次运行有效，开启时清空旧日志",
                    None,
                    "logs",
                    &mut on,
                    p,
                ) {
                    crate::logutil::set_enabled(on);
                }
                ui.add_space(14.0);
                let open_w = button_w(ui, "打开目录");
                let clear_w = button_w(ui, "清空");
                let (row, _) =
                    ui.allocate_exact_size(Vec2::new(inner.width(), 28.0), egui::Sense::hover());
                let open = egui::Rect::from_min_size(row.min, Vec2::new(open_w, 28.0));
                let clear = egui::Rect::from_min_size(
                    egui::pos2(open.right() + 8.0, row.top()),
                    Vec2::new(clear_w, 28.0),
                );
                if pill_button(ui, open, "log-open", "打开目录", p.muted, None, p).clicked() {
                    crate::logutil::open_dir();
                }
                if pill_button(ui, clear, "log-clear", "清空", p.muted, None, p).clicked() {
                    crate::logutil::clear();
                }
                ui.add_space(14.0);
                hline(ui, p);
                ui.add_space(14.0);
                let armed = reset_armed(self.reset_armed_at, Instant::now());
                repaint_when_expired(ui.ctx(), self.reset_armed_at, RESET_ARM);
                let reset_label = if armed { "确认重置" } else { "重置" };
                let bw = button_w(ui, reset_label).max(52.0);
                let row = labeled_block(
                    ui,
                    "重置配对密钥",
                    "换新密钥并断开所有手机，之后需重新扫码或输入配对码",
                    None,
                    bw,
                    p,
                );
                let b = egui::Rect::from_center_size(
                    egui::pos2(row.right() - bw / 2.0, row.center().y),
                    Vec2::new(bw, 28.0),
                );
                let (color, fill) = if armed {
                    (p.on_accent, Some(p.danger))
                } else {
                    (p.danger, None)
                };
                if pill_button(ui, b, "reset", reset_label, color, fill, p).clicked() {
                    if armed {
                        self.reset_armed_at = None;
                        crate::logutil::write(match self.state.reset_secret() {
                            Ok(()) => "pairing reset ok",
                            Err(_) => "pairing reset failed",
                        });
                    } else {
                        self.reset_armed_at = Some(Instant::now());
                        ui.ctx().request_repaint();
                    }
                }
            },
        );
    }

    fn about_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, body: egui::Rect, p: Pal) {
        let left = egui::Rect::from_min_size(body.min, Vec2::splat(QR_SIDE));
        let right = egui::Rect::from_min_size(
            egui::pos2(left.right() + PANEL_GAP, body.top()),
            Vec2::new(body.width() - QR_SIDE - PANEL_GAP, BODY_H),
        );
        paint_panel(ui, left, p);
        self.paint_brand(ui, ctx, left, p);
        let update = egui::Rect::from_min_size(right.min, Vec2::new(right.width(), 95.0));
        let machine =
            egui::Rect::from_min_max(egui::pos2(right.left(), update.bottom() + 12.0), right.max);
        paint_panel(ui, update, p);
        paint_panel(ui, machine, p);
        self.paint_update_card(ui, update, p);
        self.paint_machine_card(ui, ctx, machine, p);
    }

    fn paint_brand(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, card: egui::Rect, p: Pal) {
        let tex = self.app_icon_tex(ctx, p.dark);
        let name = ui.painter().layout_no_wrap(
            "AgentsPads".into(),
            egui::FontId::proportional(20.0),
            p.text,
        );
        let tag = ui.painter().layout_no_wrap(
            "用手机遥控桌面 Agent".into(),
            egui::FontId::proportional(12.0),
            p.muted,
        );
        let ver = crate::updater::version_label();
        let chips = [("ver", ver.as_str()), ("lic", "AGPL-3.0")];
        let chip_w: Vec<f32> = chips
            .iter()
            .map(|(_, text)| {
                ui.painter()
                    .layout_no_wrap((*text).into(), egui::FontId::proportional(11.0), p.muted)
                    .size()
                    .x
                    + 16.0
            })
            .collect();
        let chips_w = chip_w.iter().sum::<f32>() + 6.0;
        let link = format!("github.com/{}", crate::updater::GITHUB_REPO);
        let link_g =
            ui.painter()
                .layout_no_wrap(link.clone(), egui::FontId::proportional(12.0), p.accent);
        let link_w = link_g.size().x + 4.0 + 13.0;
        let stack = 72.0 + 16.0 + name.size().y + 6.0 + tag.size().y + 12.0 + 20.0 + 14.0 + 16.0;
        let mut y = card.center().y - stack / 2.0;
        let icon =
            egui::Rect::from_center_size(egui::pos2(card.center().x, y + 36.0), Vec2::splat(72.0));
        ui.painter().rect_filled(icon, 18, p.sel_fill);
        ui.put(
            icon,
            egui::Image::new((tex.id(), Vec2::splat(72.0))).corner_radius(18),
        );
        y += 72.0 + 16.0;
        ui.painter().galley(
            egui::pos2(card.center().x - name.size().x / 2.0, y),
            name.clone(),
            p.text,
        );
        y += name.size().y + 6.0;
        ui.painter().galley(
            egui::pos2(card.center().x - tag.size().x / 2.0, y),
            tag.clone(),
            p.muted,
        );
        y += tag.size().y + 12.0;
        let mut x = card.center().x - chips_w / 2.0;
        for (i, ((id, text), w)) in chips.iter().zip(chip_w).enumerate() {
            let r = egui::Rect::from_min_size(egui::pos2(x, y), Vec2::new(w, 20.0));
            ui.painter().rect_filled(r, 8, p.inset);
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                *text,
                egui::FontId::proportional(11.0),
                p.muted,
            );
            x += w;
            if i == 0 {
                x += 6.0;
            }
            let _ = id;
        }
        y += 20.0 + 14.0;
        let link_r = egui::Rect::from_min_size(
            egui::pos2(card.center().x - link_w / 2.0, y),
            Vec2::new(link_w, 16.0),
        );
        let resp = ui
            .interact(link_r, ui.id().with("repo-link"), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.painter().galley(
            egui::pos2(link_r.left(), link_r.center().y - link_g.size().y / 2.0),
            link_g,
            p.accent,
        );
        paint_icon(
            ui,
            egui::pos2(link_r.right() - 6.5, link_r.center().y),
            ICON_OPEN_IN_NEW,
            13.0,
            p.accent,
        );
        if resp.hovered() {
            ui.painter().hline(
                (link_r.left())..=(link_r.right() - 18.0),
                link_r.bottom(),
                egui::Stroke::new(1.0, p.accent),
            );
        }
        if resp.clicked() {
            ctx.open_url(egui::OpenUrl::new_tab(format!(
                "https://github.com/{}",
                crate::updater::GITHUB_REPO
            )));
        }
    }

    fn app_icon_tex(&mut self, ctx: &egui::Context, dark: bool) -> TextureHandle {
        if self.app_icon.as_ref().map(|(d, _)| *d) != Some(dark) {
            let bytes = if dark { ICON_BLACK_PNG } else { ICON_WHITE_PNG };
            let icon = eframe::icon_data::from_png_bytes(bytes).expect("app icon");
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            let tex = ctx.load_texture("about-icon", image, TextureOptions::LINEAR);
            self.app_icon = Some((dark, tex));
        }
        self.app_icon.as_ref().unwrap().1.clone()
    }

    fn paint_update_card(&mut self, ui: &mut egui::Ui, card: egui::Rect, p: Pal) {
        use crate::updater::UpdateStatus;
        let inner = card.shrink(17.0);
        ui.painter().text(
            inner.left_top(),
            egui::Align2::LEFT_TOP,
            "更新",
            egui::FontId::proportional(12.0),
            p.faint,
        );
        let status = self.updater.status.lock().unwrap().clone();
        let (dot, headline, sub, action) = update_card_copy(&status, p);
        let head =
            ui.painter()
                .layout_no_wrap(headline.clone(), egui::FontId::proportional(13.0), p.text);
        let sub_g =
            ui.painter()
                .layout_no_wrap(sub.clone(), egui::FontId::proportional(12.0), p.muted);
        let block_h = head.size().y + 3.0 + sub_g.size().y;
        let y = inner.top() + 28.0;
        ui.painter().circle_filled(
            egui::pos2(inner.left() + 4.0, y + head.size().y / 2.0),
            4.0,
            dot,
        );
        ui.painter()
            .galley(egui::pos2(inner.left() + 14.0, y), head, p.text);
        ui.painter().galley(
            egui::pos2(inner.left(), y + block_h - sub_g.size().y),
            sub_g,
            p.muted,
        );
        if let Some((label, kind)) = action {
            let bw = button_w(ui, &label).max(74.0);
            let b = egui::Rect::from_min_size(
                egui::pos2(inner.right() - bw, y + block_h / 2.0 - 14.0),
                Vec2::new(bw, 28.0),
            );
            let (color, fill) = match kind {
                UpdateAction::Install => (p.on_accent, Some(p.accent)),
                UpdateAction::Check => (p.text, None),
            };
            let resp = pill_button(ui, b, "update-action", &label, color, fill, p);
            let resp = if kind == UpdateAction::Install {
                resp.on_hover_text("确认下载并安装此版本")
            } else {
                resp
            };
            if resp.clicked() {
                match (kind, &status) {
                    (UpdateAction::Install, UpdateStatus::Available(info)) => {
                        self.updater.start_update(info);
                    }
                    (UpdateAction::Install, UpdateStatus::UpdateFailed { info, .. }) => {
                        self.updater.start_update(info)
                    }
                    (UpdateAction::Check, _) => self.updater.check_for_updates(),
                    _ => {}
                }
            }
        }
    }

    fn paint_machine_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        card: egui::Rect,
        p: Pal,
    ) {
        let inner = card.shrink(17.0);
        ui.painter().text(
            inner.left_top(),
            egui::Align2::LEFT_TOP,
            "本机",
            egui::FontId::proportional(12.0),
            p.faint,
        );
        let rows = [
            ("设备名称", self.state.identity.name.clone(), false, false),
            ("设备 ID", self.state.identity.device_id.clone(), true, true),
            ("监听端口", PORT.to_string(), true, false),
        ];
        let mut y = inner.top() + 28.0;
        for (i, (label, value, mono, copy)) in rows.into_iter().enumerate() {
            let row = egui::Rect::from_min_size(
                egui::pos2(inner.left(), y),
                Vec2::new(inner.width(), 18.0),
            );
            ui.painter().text(
                row.left_center(),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(13.0),
                p.text,
            );
            let font = if mono {
                egui::FontId::monospace(13.0)
            } else {
                egui::FontId::proportional(13.0)
            };
            let reserve = if copy { 22.0 } else { 0.0 };
            let max_w = (row.width() * 0.62).max(40.0);
            let mut job = egui::text::LayoutJob::single_section(
                value.clone(),
                egui::TextFormat::simple(font, p.text),
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
            let g = ui.painter().layout_job(job);
            let text_right = row.right() - reserve;
            ui.painter().galley(
                egui::pos2(text_right - g.size().x, row.center().y - g.size().y / 2.0),
                g,
                p.text,
            );
            if copy {
                let copied = self
                    .id_copied_at
                    .is_some_and(|at| at.elapsed() < COPY_FEEDBACK);
                repaint_when_expired(ctx, self.id_copied_at, COPY_FEEDBACK);
                let hit = egui::Rect::from_center_size(
                    egui::pos2(row.right() - 8.0, row.center().y),
                    Vec2::splat(18.0),
                );
                let resp = ui
                    .interact(hit, ui.id().with("copy-id"), egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(if copied {
                        "已复制"
                    } else {
                        "复制设备 ID"
                    });
                paint_icon(
                    ui,
                    hit.center(),
                    if copied {
                        ICON_CHECK
                    } else {
                        ICON_CONTENT_COPY
                    },
                    14.0,
                    if copied { p.ok } else { p.faint },
                );
                if resp.clicked() {
                    ctx.copy_text(value);
                    self.id_copied_at = Some(Instant::now());
                    ctx.request_repaint();
                }
            }
            if i + 1 < 3 {
                y += 18.0 + 12.0;
            }
        }
    }

    fn open_guide(&mut self) {
        self.page = Page::Pair;
        self.guide_open = true;
    }

    fn close_guide(&mut self) {
        if self.guide_open {
            self.guide_open = false;
            mark_guide_seen();
        }
    }

    fn guide_t(&mut self, ctx: &egui::Context) -> f32 {
        let id = egui::Id::new("guide-pop");
        if !self.guide_seeded {
            let _ = ctx.animate_bool_with_time(id, false, GUIDE_ANIM);
            self.guide_seeded = true;
            if self.guide_open {
                ctx.request_repaint();
            }
            return 0.0;
        }
        ctx.animate_bool_with_time(id, self.guide_open, GUIDE_ANIM)
    }

    fn guide_overlay(&mut self, ctx: &egui::Context, p: Pal) {
        let t = self.guide_t(ctx);
        if t <= 0.001 {
            return;
        }
        let screen = ctx.content_rect();
        // Keep the card under the tab bar so the header stays readable.
        let top = (MARGIN_Y + BAR_H + 8.0).max((screen.height() - GUIDE_H) * 0.5 - 4.0);
        let rest = egui::Rect::from_min_size(
            egui::pos2(
                (screen.width() - GUIDE_W) * 0.5 + screen.left(),
                screen.top() + top,
            ),
            Vec2::new(GUIDE_W, GUIDE_H),
        );
        let card = lerp_rect(self.info_rect, rest, t);
        egui::Area::new(egui::Id::new("guide-overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                let dim = ui.interact(screen, ui.id().with("guide-dim"), egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::BLACK.linear_multiply(0.60 * t),
                );
                let shadow = egui::epaint::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: egui::Color32::from_black_alpha((90.0 * t) as u8),
                };
                ui.painter().add(shadow.as_shape(card, 16));
                ui.painter().rect(
                    card,
                    16,
                    with_alpha(p.card, t),
                    egui::Stroke::new(1.0, with_alpha(p.line, t)),
                    egui::StrokeKind::Inside,
                );
                let _ = ui.interact(card, ui.id().with("guide-card"), egui::Sense::click());
                let prev = ui.clip_rect();
                ui.set_clip_rect(prev.intersect(card));
                let close = paint_guide(ui, card, t, p);
                ui.set_clip_rect(prev);
                let outside = dim.clicked()
                    && dim
                        .interact_pointer_pos()
                        .is_some_and(|pos| !card.contains(pos));
                if close || outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.close_guide();
                }
            });
    }

    /// Modal confirmation: the admin instance uses its own pairing key, so phones
    /// must pair again after switching either way.
    #[cfg(windows)]
    fn admin_confirm_overlay(&mut self, ctx: &egui::Context, p: Pal) {
        let Some(on) = self.admin_confirm else {
            return;
        };
        let (title, body) = if on {
            (
                "切换到管理员模式",
                "如果切换的话，手机上需要重新配对。\n确认后将请求一次系统授权，并自动重启程序。",
            )
        } else {
            (
                "关闭管理员模式",
                "如果切换的话，手机上需要重新配对。\n确认后将停止管理员实例、清理管理员任务，并以普通权限重新打开程序。",
            )
        };
        let screen = ctx.content_rect();
        let mut choice = None;
        egui::Area::new(egui::Id::new("admin-confirm"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                // Swallow clicks so nothing underneath reacts while asking.
                let _ = ui.interact(screen, ui.id().with("dim"), egui::Sense::click());
                ui.painter()
                    .rect_filled(screen, 0.0, egui::Color32::BLACK.linear_multiply(0.60));
                let (w, pad) = (340.0, 20.0);
                let head = ui.painter().layout_no_wrap(
                    title.into(),
                    egui::FontId::proportional(15.0),
                    p.text,
                );
                let text = ui.painter().layout(
                    body.into(),
                    egui::FontId::proportional(13.0),
                    p.muted,
                    w - 2.0 * pad,
                );
                let h = pad + head.size().y + 10.0 + text.size().y + 18.0 + 28.0 + pad;
                let card = egui::Rect::from_center_size(screen.center(), Vec2::new(w, h));
                ui.painter().add(
                    egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(90),
                    }
                    .as_shape(card, 16),
                );
                ui.painter().rect(
                    card,
                    16,
                    p.card,
                    egui::Stroke::new(1.0, p.line),
                    egui::StrokeKind::Inside,
                );
                let _ = ui.interact(card, ui.id().with("card"), egui::Sense::click());
                let mut y = card.top() + pad;
                let x = card.left() + pad;
                let head_h = head.size().y;
                ui.painter().galley(egui::pos2(x, y), head, p.text);
                y += head_h + 10.0;
                ui.painter().galley(egui::pos2(x, y), text, p.muted);
                let row_y = card.bottom() - pad - 28.0;
                let ok = egui::Rect::from_min_size(
                    egui::pos2(card.right() - pad - 64.0, row_y),
                    Vec2::new(64.0, 28.0),
                );
                let cancel = ok.translate(Vec2::new(-72.0, 0.0));
                if pill_button(ui, cancel, "admin-cancel", "取消", p.muted, None, p).clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    choice = Some(false);
                }
                if pill_button(ui, ok, "admin-ok", "确认", p.on_accent, Some(p.accent), p).clicked()
                {
                    choice = Some(true);
                }
            });
        if let Some(confirmed) = choice {
            self.admin_confirm = None;
            if confirmed {
                self.set_run_as_admin(on);
                // 交接进程没拉起来时，这一帧设置页已经画完，补一帧把警告画出来。
                if crate::elevation::handoff_failure().is_some() {
                    ctx.request_repaint();
                }
            }
        }
    }

    #[cfg(windows)]
    fn set_run_as_admin(&mut self, on: bool) {
        use crate::elevation;
        // 开机启动的当前选择随任务带过去；管理员模式下它由计划任务的登录触发负责。
        if on {
            if let Err(reason) = elevation::install_task(crate::autostart::enabled()) {
                elevation::note_handoff_failure(reason);
                crate::logutil::write("admin task install failed");
                return;
            }
        }
        if !on {
            if let Err(reason) = elevation::begin_disable_admin() {
                elevation::note_handoff_failure(&reason);
                crate::logutil::write("admin disable handoff failed");
                self.run_as_admin = elevation::enabled();
                return;
            }
            crate::handle::stop_input();
            #[cfg(all(windows, feature = "perf-probe"))]
            if let Some(probe) = crate::perf_probe::active() {
                probe.record(
                    "stop_input_done",
                    Some("admin_mode_switch"),
                    None,
                    None,
                    None,
                    None,
                );
            }
            exit_with_probe(0, "admin_mode_switch");
        }
        // 取消 UAC 时受保护标记仍在，开关保持打开。
        self.run_as_admin = true;
        // 撤掉用户 Startup 里的旧项（管理员模式下由登录触发接管）；提权进程里这是空操作。
        crate::autostart::apply();
        self.autostart_enabled = crate::autostart::enabled();
        if on && self.run_as_admin && !elevation::is_elevated() {
            // 新的普通进程等本进程退出、端口释放后再走 main 里的交接。
            // schtasks /Run 成功只代表任务已排队，不能在这里直接退出。
            if elevation::spawn_unelevated_handoff() {
                crate::handle::stop_input();
                #[cfg(all(windows, feature = "perf-probe"))]
                if let Some(probe) = crate::perf_probe::active() {
                    probe.record(
                        "stop_input_done",
                        Some("admin_mode_handoff"),
                        None,
                        None,
                        None,
                        None,
                    );
                }
                exit_with_probe(0, "admin_mode_handoff");
            }
            elevation::note_handoff_failure("无法重新打开窗口");
            crate::logutil::write("admin handoff spawn failed");
        }
    }
}

#[derive(Clone, Copy)]
struct Pal {
    dark: bool,
    page: egui::Color32,
    card: egui::Color32,
    inset: egui::Color32,
    hover: egui::Color32,
    line: egui::Color32,
    text: egui::Color32,
    muted: egui::Color32,
    faint: egui::Color32,
    sel_fill: egui::Color32,
    accent: egui::Color32,
    on_accent: egui::Color32,
    ok: egui::Color32,
    warn: egui::Color32,
    danger: egui::Color32,
}

impl Pal {
    fn new(dark: bool) -> Self {
        let c = egui::Color32::from_rgb;
        if dark {
            Self {
                dark,
                page: c(16, 16, 18),
                card: c(26, 26, 29),
                inset: c(32, 32, 36),
                hover: c(44, 44, 49),
                line: c(46, 46, 52),
                text: c(236, 236, 240),
                muted: c(160, 160, 168),
                faint: c(112, 112, 120),
                sel_fill: c(32, 58, 86),
                accent: c(94, 175, 249),
                on_accent: c(12, 12, 14),
                ok: c(74, 201, 115),
                warn: c(232, 166, 72),
                danger: c(240, 98, 98),
            }
        } else {
            Self {
                dark,
                page: c(244, 244, 246),
                card: c(255, 255, 255),
                inset: c(247, 247, 249),
                hover: c(236, 236, 240),
                line: c(226, 226, 231),
                text: c(24, 24, 27),
                muted: c(102, 102, 110),
                faint: c(146, 146, 154),
                sel_fill: c(224, 239, 252),
                accent: c(36, 140, 236),
                on_accent: c(255, 255, 255),
                ok: c(42, 168, 84),
                warn: c(196, 124, 30),
                danger: c(214, 64, 64),
            }
        }
    }
}

fn paint_panel(ui: &egui::Ui, rect: egui::Rect, p: Pal) {
    ui.painter().rect(
        rect,
        CARD_R,
        p.card,
        egui::Stroke::new(1.0, p.line),
        egui::StrokeKind::Inside,
    );
}

fn kicker(ui: &mut egui::Ui, text: &str, p: Pal) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 16.0), egui::Sense::hover());
    ui.painter().text(
        r.left_center(),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(12.0),
        p.faint,
    );
}

fn hline(ui: &mut egui::Ui, p: Pal) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, egui::Stroke::new(1.0, p.line));
}

fn button_w(ui: &egui::Ui, label: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            label.into(),
            egui::FontId::proportional(12.5),
            egui::Color32::WHITE,
        )
        .size()
        .x
        + 26.0
}

fn labeled_block(
    ui: &mut egui::Ui,
    label: &str,
    desc: &str,
    help: Option<&str>,
    reserve: f32,
    p: Pal,
) -> egui::Rect {
    let width = ui.available_width();
    let text_w = (width - reserve - 12.0).max(40.0);
    let label_g =
        ui.painter()
            .layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), p.text);
    let mut job = egui::text::LayoutJob::single_section(
        desc.to_string(),
        egui::TextFormat::simple(egui::FontId::proportional(12.0), p.muted),
    );
    job.wrap.max_width = text_w;
    let desc_g = ui.painter().layout_job(job);
    let h = label_g.size().y + 3.0 + desc_g.size().y;
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, h), egui::Sense::hover());
    ui.painter()
        .galley(egui::pos2(row.left(), row.top()), label_g.clone(), p.text);
    if let Some(help) = help {
        let r = egui::Rect::from_center_size(
            egui::pos2(
                row.left() + label_g.size().x + 14.0,
                row.top() + label_g.size().y / 2.0,
            ),
            Vec2::splat(18.0),
        );
        let resp = ui
            .interact(r, ui.id().with(("help", label)), egui::Sense::hover())
            .on_hover_text(help);
        let color = if resp.hovered() { p.text } else { p.faint };
        paint_icon(ui, r.center(), ICON_HELP, 15.0, color);
    }
    ui.painter().galley(
        egui::pos2(row.left(), row.top() + label_g.size().y + 3.0),
        desc_g,
        p.muted,
    );
    row
}

#[cfg(windows)]
fn admin_handoff_warning(ui: &mut egui::Ui, p: Pal) {
    let Some(reason) = crate::elevation::handoff_failure() else {
        return;
    };
    ui.add_space(8.0);
    let privilege = if crate::elevation::is_elevated() {
        "当前已提权"
    } else {
        "当前普通权限"
    };
    let text = format!("管理员模式操作未完成，{privilege}：{reason}");
    let width = ui.available_width();
    let mut job = egui::text::LayoutJob::single_section(
        text,
        egui::TextFormat::simple(egui::FontId::proportional(12.0), p.warn),
    );
    job.wrap.max_width = width;
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, galley.size().y), egui::Sense::hover());
    ui.painter().galley(rect.min, galley, p.warn);
}

fn switch_block(
    ui: &mut egui::Ui,
    label: &str,
    desc: &str,
    help: Option<&str>,
    id: &str,
    on: &mut bool,
    p: Pal,
) -> bool {
    let row = labeled_block(ui, label, desc, help, 40.0, p);
    let sw = egui::Rect::from_min_size(
        egui::pos2(row.right() - 40.0, row.center().y - 12.0),
        Vec2::new(40.0, 24.0),
    );
    toggle(ui, sw, id, on, p)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateAction {
    Check,
    Install,
}

fn update_card_copy(
    status: &crate::updater::UpdateStatus,
    p: Pal,
) -> (
    egui::Color32,
    String,
    String,
    Option<(String, UpdateAction)>,
) {
    use crate::updater::UpdateStatus;
    let schedule = "启动时及每 24 小时自动检查".to_string();
    let check =
        || crate::updater::checks_enabled().then(|| ("检查更新".to_string(), UpdateAction::Check));
    match status {
        UpdateStatus::Idle if !crate::updater::checks_enabled() => {
            (p.faint, "调试构建不检查更新".into(), schedule.clone(), None)
        }
        UpdateStatus::Idle => (p.faint, "尚未检查".into(), schedule, check()),
        UpdateStatus::Checking => (p.faint, "检查中…".into(), schedule, None),
        UpdateStatus::UpToDate => (p.ok, "已是最新版本".into(), schedule, check()),
        UpdateStatus::Available(info) => (
            p.ok,
            format!("发现新版本 v{}", info.version),
            schedule,
            Some((format!("更新到 v{}", info.version), UpdateAction::Install)),
        ),
        UpdateStatus::Updating(msg) => (p.accent, msg.clone(), schedule, None),
        UpdateStatus::UpdateFailed { .. } => (
            p.warn,
            "更新失败".into(),
            schedule,
            Some(("重试".into(), UpdateAction::Install)),
        ),
        UpdateStatus::Failed(_) => (
            p.warn,
            "检查失败".into(),
            schedule,
            Some(("重试".into(), UpdateAction::Check)),
        ),
    }
}

fn paint_nic_row(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    index: usize,
    nic: &Nic,
    selected: bool,
    p: Pal,
) -> bool {
    let resp = ui
        .interact(rect, ui.id().with(("nic", index)), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let (fill, stroke) = if selected {
        (p.sel_fill, egui::Stroke::new(1.0, p.accent))
    } else if resp.hovered() {
        (p.hover, egui::Stroke::new(1.0, p.line))
    } else {
        (p.inset, egui::Stroke::new(1.0, p.line))
    };
    ui.painter()
        .rect(rect, 10, fill, stroke, egui::StrokeKind::Inside);
    let (icon, kind) = nic_kind_spec(nic.kind);
    paint_icon(
        ui,
        egui::pos2(rect.left() + 22.0, rect.center().y),
        icon,
        17.0,
        if selected { p.accent } else { p.muted },
    );
    let radio = egui::pos2(rect.right() - 16.0, rect.center().y);
    if selected {
        ui.painter().circle_filled(radio, 8.0, p.accent);
        paint_icon(ui, radio, ICON_CHECK, 12.0, p.on_accent);
    } else {
        ui.painter()
            .circle_stroke(radio, 7.5, egui::Stroke::new(1.5, p.faint));
    }
    let ip = ui
        .painter()
        .layout_no_wrap(nic.ip.clone(), egui::FontId::monospace(13.0), p.text);
    let ip_left = radio.x - 14.0 - ip.size().x;
    ui.painter().galley(
        egui::pos2(ip_left, rect.center().y - ip.size().y / 2.0),
        ip,
        p.text,
    );
    let name_left = rect.left() + 42.0;
    let kind_g =
        ui.painter()
            .layout_no_wrap(kind.to_string(), egui::FontId::proportional(13.0), p.text);
    let kind_w = kind_g.size().x;
    ui.painter().galley(
        egui::pos2(name_left, rect.center().y - kind_g.size().y / 2.0),
        kind_g,
        p.text,
    );
    let room = ip_left - 10.0 - (name_left + kind_w + 6.0);
    if room > 20.0 {
        let mut job = egui::text::LayoutJob::single_section(
            nic.name.clone(),
            egui::TextFormat::simple(egui::FontId::proportional(12.0), p.faint),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
        let name_g = ui.painter().layout_job(job);
        ui.painter().galley(
            egui::pos2(
                name_left + kind_w + 6.0,
                rect.center().y - name_g.size().y / 2.0,
            ),
            name_g,
            p.faint,
        );
    }
    resp.clicked()
}

fn paint_empty_slot(ui: &mut egui::Ui, ctx: &egui::Context, rect: egui::Rect, p: Pal) {
    let resp = ui.interact(rect, ui.id().with("nic-empty"), egui::Sense::hover());
    let t = ctx.animate_bool_with_time(egui::Id::new("nic-empty-anim"), resp.hovered(), EMPTY_ANIM);
    if t > 0.01 {
        ui.painter()
            .rect_filled(rect, 10, with_alpha(p.sel_fill, t));
    }
    let stroke = p.line.lerp_to_gamma(p.accent, t);
    paint_dashed_round_rect(
        ui.painter(),
        rect.shrink(0.5),
        10.0,
        egui::Stroke::new(1.0, stroke),
    );
    let prev = ui.clip_rect();
    ui.set_clip_rect(prev.intersect(rect));
    paint_empty_caption(
        ui,
        rect,
        1.0 - t,
        4.0 * t,
        ICON_ADD,
        (p.faint, p.faint),
        "连接其他网络后，新网卡会自动出现在这里",
    );
    paint_empty_caption(
        ui,
        rect,
        t,
        -4.0 * (1.0 - t),
        ICON_SMARTPHONE,
        (p.accent, p.text),
        "手机端「扫码添加」，扫不了就输入地址和配对码",
    );
    ui.set_clip_rect(prev);
}

fn paint_empty_caption(
    ui: &egui::Ui,
    rect: egui::Rect,
    alpha: f32,
    dy: f32,
    icon: MaterialIcon,
    colors: (egui::Color32, egui::Color32),
    text: &str,
) {
    if alpha < 0.02 {
        return;
    }
    let (icon_color, text_color) = colors;
    let max_w = (rect.width() - 36.0).max(8.0);
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat::simple(
            egui::FontId::proportional(12.0),
            with_alpha(text_color, alpha),
        ),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
    let g = ui.painter().layout_job(job);
    let group = 16.0 + 6.0 + g.size().x;
    let x = rect.center().x - group / 2.0;
    let y = rect.center().y + dy;
    paint_icon(
        ui,
        egui::pos2(x + 8.0, y),
        icon,
        16.0,
        with_alpha(icon_color, alpha),
    );
    ui.painter().galley(
        egui::pos2(x + 22.0, y - g.size().y / 2.0),
        g,
        with_alpha(text_color, alpha),
    );
}

fn paint_dashed_round_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    stroke: egui::Stroke,
) {
    let r = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
    if r < 0.5 || rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    let n = 6;
    let mut pts = Vec::with_capacity(4 * n + 8);
    pts.push(egui::pos2(rect.left() + r, rect.top()));
    pts.push(egui::pos2(rect.right() - r, rect.top()));
    push_arc(
        &mut pts,
        rect.right() - r,
        rect.top() + r,
        r,
        -std::f32::consts::FRAC_PI_2,
        0.0,
        n,
    );
    pts.push(egui::pos2(rect.right(), rect.bottom() - r));
    push_arc(
        &mut pts,
        rect.right() - r,
        rect.bottom() - r,
        r,
        0.0,
        std::f32::consts::FRAC_PI_2,
        n,
    );
    pts.push(egui::pos2(rect.left() + r, rect.bottom()));
    push_arc(
        &mut pts,
        rect.left() + r,
        rect.bottom() - r,
        r,
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
        n,
    );
    pts.push(egui::pos2(rect.left(), rect.top() + r));
    push_arc(
        &mut pts,
        rect.left() + r,
        rect.top() + r,
        r,
        std::f32::consts::PI,
        std::f32::consts::PI * 1.5,
        n,
    );
    pts.push(pts[0]);
    for shape in egui::Shape::dashed_line(&pts, stroke, 4.0, 4.0) {
        painter.add(shape);
    }
}

fn push_arc(pts: &mut Vec<egui::Pos2>, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, n: usize) {
    for i in 1..=n {
        let a = a0 + (a1 - a0) * (i as f32 / n as f32);
        pts.push(egui::pos2(cx + r * a.cos(), cy + r * a.sin()));
    }
}

fn paint_guide(ui: &mut egui::Ui, card: egui::Rect, t: f32, p: Pal) -> bool {
    let mut close = false;
    let left = card.left() + 20.0;
    let right = card.right() - 20.0;
    let mut y = card.top() + 20.0;
    ui.painter().text(
        egui::pos2(left, y + 14.0),
        egui::Align2::LEFT_CENTER,
        "开始连接手机",
        egui::FontId::proportional(16.0),
        with_alpha(p.text, t),
    );
    let x = egui::Rect::from_min_size(egui::pos2(right - 28.0, y), Vec2::splat(28.0));
    let x_resp = ui
        .interact(x, ui.id().with("guide-x"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if x_resp.hovered() {
        ui.painter().rect_filled(x, 8, with_alpha(p.hover, t));
    }
    paint_icon(ui, x.center(), ICON_CLOSE, 14.0, with_alpha(p.muted, t));
    if x_resp.clicked() {
        close = true;
    }
    y += 28.0 + 16.0;
    for (n, title, body) in [
        (
            "1",
            "手机与电脑在同一网络",
            "同一 Wi-Fi / 局域网，或同一个虚拟组网",
        ),
        (
            "2",
            "手机端「扫码添加」",
            "对准左侧二维码，扫到即自动保存这台电脑",
        ),
        (
            "3",
            "扫不了就手动输入",
            "在手机上填地址和配对码；配对码窗口隐藏后失效",
        ),
    ] {
        let badge = egui::Rect::from_min_size(egui::pos2(left, y + 4.0), Vec2::splat(24.0));
        ui.painter()
            .circle_filled(badge.center(), 12.0, with_alpha(p.accent, t));
        ui.painter().text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            n,
            egui::FontId::proportional(12.0),
            with_alpha(p.on_accent, t),
        );
        let tx = badge.right() + 12.0;
        ui.painter().text(
            egui::pos2(tx, y),
            egui::Align2::LEFT_TOP,
            title,
            egui::FontId::proportional(13.0),
            with_alpha(p.text, t),
        );
        ui.painter().text(
            egui::pos2(tx, y + 18.0),
            egui::Align2::LEFT_TOP,
            body,
            egui::FontId::proportional(12.0),
            with_alpha(p.muted, t),
        );
        y += 32.0 + 16.0;
    }
    let foot_y = card.bottom() - 20.0 - 30.0;
    paint_icon(
        ui,
        egui::pos2(left + 7.0, foot_y + 15.0),
        ICON_INFO,
        14.0,
        with_alpha(p.faint, t),
    );
    ui.painter().text(
        egui::pos2(left + 20.0, foot_y + 15.0),
        egui::Align2::LEFT_CENTER,
        "以后可点左上角按钮再次查看",
        egui::FontId::proportional(12.0),
        with_alpha(p.faint, t),
    );
    let ok = egui::Rect::from_min_size(egui::pos2(right - 72.0, foot_y), Vec2::new(72.0, 30.0));
    if pill_button(
        ui,
        ok,
        "guide-ok",
        "知道了",
        with_alpha(p.on_accent, t),
        Some(with_alpha(p.accent, t)),
        p,
    )
    .clicked()
    {
        close = true;
    }
    close
}

fn with_alpha(color: egui::Color32, t: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        color.r(),
        color.g(),
        color.b(),
        (color.a() as f32 * t.clamp(0.0, 1.0)) as u8,
    )
}

fn lerp_rect(a: egui::Rect, b: egui::Rect, t: f32) -> egui::Rect {
    egui::Rect::from_min_max(a.min.lerp(b.min, t), a.max.lerp(b.max, t))
}

fn guide_flag_path() -> Option<std::path::PathBuf> {
    crate::identity::runtime_dir().map(|dir| dir.join("guide_seen"))
}

fn guide_seen() -> bool {
    let state_flag = guide_flag_path().is_some_and(|path| flag_exists(&path));
    if state_flag {
        return true;
    }
    let elevated = process_elevated();
    let user_flag =
        elevated && flag_exists(&crate::identity::fixed_user_data_dir().join("guide_seen"));
    guide_already_seen(false, elevated, user_flag)
}

fn mark_guide_seen() {
    let Some(path) = guide_flag_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, b"1");
}

fn flag_exists(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
}

/// 提权实例自己的标记优先。没有时，只把用户目录里已有的引导标记当成「看过」。
fn guide_already_seen(state_flag: bool, elevated: bool, user_flag: bool) -> bool {
    state_flag || (elevated && user_flag)
}

fn process_elevated() -> bool {
    crate::elevation::is_elevated()
}

fn small(text: impl Into<String>, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text.into()).size(12.0).color(color)
}

fn link(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    let resp = ui
        .add(egui::Label::new(small(text, color)).sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() {
        let r = resp.rect;
        ui.painter()
            .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, color));
    }
    resp
}

fn paint_icon(
    ui: &egui::Ui,
    center: egui::Pos2,
    icon: MaterialIcon,
    size: f32,
    color: egui::Color32,
) {
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        icon.codepoint,
        egui::FontId::new(size, icon.font_family()),
        color,
    );
}

fn pill_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: &str,
    label: &str,
    color: egui::Color32,
    fill: Option<egui::Color32>,
    p: Pal,
) -> egui::Response {
    let resp = ui
        .interact(rect, ui.id().with(id), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let bg = match fill {
        Some(f) if resp.hovered() => f.gamma_multiply(0.88),
        Some(f) => f,
        None if resp.hovered() => p.hover,
        None => egui::Color32::TRANSPARENT,
    };
    let stroke = if fill.is_some() {
        egui::Stroke::NONE
    } else {
        egui::Stroke::new(1.0, p.line)
    };
    ui.painter()
        .rect(rect, 8, bg, stroke, egui::StrokeKind::Inside);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.5),
        color,
    );
    resp
}

#[allow(clippy::too_many_arguments)]
fn tile(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: &str,
    label: &str,
    value: &str,
    value_color: egui::Color32,
    icon: MaterialIcon,
    p: Pal,
) -> egui::Response {
    let resp = ui
        .interact(rect, ui.id().with(id), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if resp.hovered() { p.hover } else { p.card };
    ui.painter().rect(
        rect,
        12,
        fill,
        egui::Stroke::new(1.0, p.line),
        egui::StrokeKind::Inside,
    );
    let left = rect.left() + 12.0;
    ui.painter().text(
        egui::pos2(left, rect.top() + 10.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::proportional(11.5),
        p.muted,
    );
    ui.painter().text(
        egui::pos2(left, rect.bottom() - 10.0),
        egui::Align2::LEFT_BOTTOM,
        value,
        egui::FontId::monospace(14.0),
        value_color,
    );
    let icon_color = if resp.hovered() { p.text } else { p.faint };
    paint_icon(
        ui,
        egui::pos2(rect.right() - 18.0, rect.top() + 18.0),
        icon,
        15.0,
        icon_color,
    );
    resp
}

fn toggle(ui: &mut egui::Ui, rect: egui::Rect, id: &str, on: &mut bool, p: Pal) -> bool {
    let id = ui.id().with(id);
    let resp = ui
        .interact(rect, id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        *on = !*on;
    }
    let t = ui.ctx().animate_bool_responsive(id, *on);
    let off = if p.dark {
        egui::Color32::from_rgb(64, 64, 72)
    } else {
        egui::Color32::from_rgb(208, 208, 216)
    };
    let mut track = off.lerp_to_gamma(p.accent, t);
    if resp.hovered() {
        track = track.gamma_multiply(0.9);
    }
    let rad = rect.height() / 2.0;
    ui.painter().rect_filled(rect, rad, track);
    let x = egui::lerp((rect.left() + rad)..=(rect.right() - rad), t);
    ui.painter().circle_filled(
        egui::pos2(x, rect.center().y),
        rad - 2.5,
        egui::Color32::WHITE,
    );
    resp.clicked()
}

fn fitted_height(content: f32, monitor: Option<f32>) -> f32 {
    let max = monitor.map_or(WINDOW_MAX_H, |m| (m - 80.0).max(320.0));
    content.min(max).ceil()
}

/// Height left under the address tiles and the NIC title, so the list bottom
/// meets the QR card. Three rows fill it; extra rows scroll inside.
const fn nic_list_height() -> f32 {
    QR_SIDE - TILE_H - COL_AFTER_TILE - NIC_TITLE_H - COL_AFTER_TITLE
}

const fn nic_track_h() -> f32 {
    nic_list_height() - NIC_LIST_STROKE * 2.0 - NIC_LIST_PAD_Y * 2.0
}

fn nic_row_w(panel_w: f32) -> f32 {
    panel_w
        - NIC_LIST_STROKE * 2.0
        - NIC_LIST_PAD_X
        - NIC_SCROLL_GAP
        - NIC_SCROLL_W
        - NIC_LIST_PAD_R
}

fn nic_rows_h(n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * NIC_H + (n - 1) as f32 * NIC_GAP
    }
}

fn empty_slot_h(n: usize) -> f32 {
    if n >= 3 {
        0.0
    } else {
        let used = if n == 0 { 0.0 } else { nic_rows_h(n) + NIC_GAP };
        (nic_track_h() - used).max(0.0)
    }
}

fn nic_counter(nics: &[Nic], ip: &str) -> String {
    match nics.iter().position(|n| n.ip == ip) {
        Some(i) => format!("第 {} / {} 张", i + 1, nics.len()),
        None => format!("共 {} 张", nics.len()),
    }
}

fn nic_kind_spec(kind: NicKind) -> (MaterialIcon, &'static str) {
    match kind {
        NicKind::Wifi => (ICON_WIFI, "Wi-Fi"),
        NicKind::Ethernet => (ICON_LAN, "以太网"),
        NicKind::Tunnel => (ICON_VPN_LOCK, "隧道"),
        NicKind::Virtual => (ICON_HUB, "虚拟网卡"),
        NicKind::Other => (ICON_SETTINGS_ETHERNET, "网卡"),
    }
}

fn theme_icon_spec(theme: ThemePreference) -> (MaterialIcon, &'static str) {
    match theme {
        ThemePreference::System => (ICON_COMPUTER, "跟随系统"),
        ThemePreference::Light => (ICON_LIGHT_MODE, "浅色模式"),
        ThemePreference::Dark => (ICON_DARK_MODE, "深色模式"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionNotice {
    DevelopmentLaunch,
    NeedsAccess,
}

const ICON_WHITE_PNG: &[u8] = include_bytes!("../assets/icon_white.png");
const ICON_BLACK_PNG: &[u8] = include_bytes!("../assets/icon_black.png");

fn permission_notice(packaged: bool, ax_ok: bool) -> Option<PermissionNotice> {
    if !packaged {
        Some(PermissionNotice::DevelopmentLaunch)
    } else if !ax_ok {
        Some(PermissionNotice::NeedsAccess)
    } else {
        None
    }
}

fn copy_label(copied_at: Option<Instant>, now: Instant) -> &'static str {
    if copied_at.is_some_and(|at| now.duration_since(at) < COPY_FEEDBACK) {
        "已复制"
    } else {
        "复制地址"
    }
}

fn reset_armed(armed_at: Option<Instant>, now: Instant) -> bool {
    armed_at.is_some_and(|t| now.duration_since(t) < RESET_ARM)
}

/// egui may fire a delayed repaint slightly early, so re-arm until really expired.
fn repaint_when_expired(ctx: &egui::Context, since: Option<Instant>, ttl: Duration) {
    if let Some(left) = since.and_then(|t| ttl.checked_sub(t.elapsed())) {
        ctx.request_repaint_after(left);
    }
}

fn apply_app_style(ctx: &egui::Context) {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = Vec2::new(8.0, 8.0);
            style.spacing.button_padding = Vec2::new(12.0, 7.0);
            style.spacing.interact_size.y = 34.0;
            style.spacing.tooltip_width = TOOLTIP_W;
            style.interaction.tooltip_delay = 0.0;
            style.interaction.show_tooltips_only_when_still = false;
            for widget in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.open,
            ] {
                widget.corner_radius = 8.into();
                widget.expansion = 0.0;
            }
        });
    }
}

fn theme_from_str(s: &str) -> ThemePreference {
    match s {
        "light" => ThemePreference::Light,
        "dark" => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

fn theme_to_str(p: ThemePreference) -> &'static str {
    match p {
        ThemePreference::Light => "light",
        ThemePreference::Dark => "dark",
        ThemePreference::System => "system",
    }
}

/// CJK fonts (Hiragino, YaHei) and the icon font sit at different heights in
/// egui's line box, so centered text looks high and icons low. Measure each
/// font's ink offset once and cancel it with `FontTweak`, whatever font loaded.
fn install_fonts(ctx: &egui::Context) {
    let cjk = cjk_font_bytes();
    if cjk.is_none() {
        crate::logutil::write("no CJK font found");
    }
    let icons = egui_material_icons::font_insert();
    let build = |cjk_dy: f32, icon_dy: f32| {
        let mut defs = FontDefinitions::default();
        if let Some(bytes) = cjk {
            let mut data = FontData::from_static(bytes);
            data.tweak.y_offset_factor = cjk_dy;
            defs.font_data.insert("cjk".into(), Arc::new(data));
            defs.families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".into());
            defs.families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("cjk".into());
        }
        let mut data = icons.data.clone();
        data.tweak.y_offset_factor += icon_dy;
        defs.font_data.insert(icons.name.clone(), Arc::new(data));
        for f in &icons.families {
            let list = defs.families.entry(f.family.clone()).or_default();
            if matches!(f.priority, egui::epaint::text::FontPriority::Highest) {
                list.insert(0, icons.name.clone());
            } else {
                list.push(icons.name.clone());
            }
        }
        defs
    };
    let probe = egui::Context::default();
    probe.set_fonts(build(0.0, 0.0));
    probe.begin_pass(Default::default());
    let cjk_dy = if cjk.is_some() {
        -ink_offset_em(&probe, "中", egui::FontId::proportional(PROBE_PT))
    } else {
        0.0
    };
    let icon_dy = -ink_offset_em(
        &probe,
        ICON_HELP.codepoint,
        egui::FontId::new(PROBE_PT, ICON_HELP.font_family()),
    );
    ctx.set_fonts(build(cjk_dy, icon_dy));
}

const PROBE_PT: f32 = 64.0;

/// Vertical distance from the line box center to the ink center, in em.
fn ink_offset_em(ctx: &egui::Context, text: &str, font: egui::FontId) -> f32 {
    let size = font.size;
    let g = ctx.fonts_mut(|f| f.layout_no_wrap(text.into(), font, egui::Color32::WHITE));
    let (mut top, mut bottom) = (f32::MAX, f32::MIN);
    for glyph in g.rows.iter().flat_map(|r| &r.row.glyphs) {
        if glyph.uv_rect.size.y > 0.0 {
            let t = glyph.pos.y + glyph.uv_rect.offset.y;
            top = top.min(t);
            bottom = bottom.max(t + glyph.uv_rect.size.y);
        }
    }
    if top > bottom {
        return 0.0;
    }
    ((top + bottom) / 2.0 - g.rect.center().y) / size
}

/// Leaked: egui clones `FontData` per font rebuild, and a borrowed slice
/// clones as a pointer while an owned `Vec` would copy the whole file.
fn cjk_font_bytes() -> Option<&'static [u8]> {
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/PrivateFrameworks/FontServices.framework/Versions/A/Resources/Reserved/PingFangUI.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            crate::logutil::write("font load ok");
            return Some(bytes.leak());
        }
    }
    None
}

fn tray_icon_data(dark: bool) -> egui::IconData {
    let bytes = if dark { ICON_BLACK_PNG } else { ICON_WHITE_PNG };
    eframe::icon_data::from_png_bytes(bytes).expect("valid tray icon")
}

fn tray_icon(dark: bool) -> Icon {
    let icon = tray_icon_data(dark);
    Icon::from_rgba(icon.rgba, icon.width, icon.height).expect("valid tray icon")
}

/// Left button-up shows the window. A double-click arrives as its own event
/// between the two ups (not a second down); showing on it is idempotent.
/// Right-click stays the menu.
#[cfg(any(windows, test))]
fn tray_click_shows_window(ev: &TrayIconEvent) -> bool {
    matches!(
        ev,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        }
    )
}

/// Restore immediately from the native tray callback; App::logic then syncs egui.
#[cfg(windows)]
fn reveal_pairing_window(slot: &AtomicIsize) {
    let raw = slot.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };
    let hwnd = HWND(raw as *mut std::ffi::c_void);
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_buttons_use_material_icons_and_tooltips() {
        let (system, system_tip) = theme_icon_spec(ThemePreference::System);
        let (light, light_tip) = theme_icon_spec(ThemePreference::Light);
        let (dark, dark_tip) = theme_icon_spec(ThemePreference::Dark);
        assert_eq!(system.codepoint, ICON_COMPUTER.codepoint);
        assert_eq!(light.codepoint, ICON_LIGHT_MODE.codepoint);
        assert_eq!(dark.codepoint, ICON_DARK_MODE.codepoint);
        assert_eq!(system_tip, "跟随系统");
        assert_eq!(light_tip, "浅色模式");
        assert_eq!(dark_tip, "深色模式");
        assert_ne!(system.codepoint, light.codepoint);
        assert_ne!(light.codepoint, dark.codepoint);
    }

    #[test]
    fn tray_theme_icons_are_distinct_and_high_resolution() {
        let light = tray_icon_data(false);
        let dark = tray_icon_data(true);
        assert_eq!((light.width, light.height), (1024, 1024));
        assert_eq!((dark.width, dark.height), (1024, 1024));
        assert_ne!(light.rgba, dark.rgba);
    }

    #[test]
    fn tray_left_release_shows_and_right_click_does_not() {
        fn click(button: MouseButton, state: MouseButtonState) -> TrayIconEvent {
            TrayIconEvent::Click {
                id: tray_icon::TrayIconId::new("t"),
                position: Default::default(),
                rect: Default::default(),
                button,
                button_state: state,
            }
        }
        assert!(tray_click_shows_window(&click(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        assert!(!tray_click_shows_window(&click(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        assert!(!tray_click_shows_window(&click(
            MouseButton::Right,
            MouseButtonState::Up
        )));
        assert!(tray_click_shows_window(&TrayIconEvent::DoubleClick {
            id: tray_icon::TrayIconId::new("t"),
            position: Default::default(),
            rect: Default::default(),
            button: MouseButton::Left,
        }));
        assert!(!tray_click_shows_window(&TrayIconEvent::DoubleClick {
            id: tray_icon::TrayIconId::new("t"),
            position: Default::default(),
            rect: Default::default(),
            button: MouseButton::Right,
        }));
        assert!(!tray_click_shows_window(&TrayIconEvent::Enter {
            id: tray_icon::TrayIconId::new("t"),
            position: Default::default(),
            rect: Default::default(),
        }));
    }

    #[test]
    fn permission_notice_never_requests_access_for_bare_binary() {
        assert_eq!(
            permission_notice(false, false),
            Some(PermissionNotice::DevelopmentLaunch),
        );
        assert_eq!(
            permission_notice(false, true),
            Some(PermissionNotice::DevelopmentLaunch),
        );
        assert_eq!(
            permission_notice(true, false),
            Some(PermissionNotice::NeedsAccess),
        );
        assert_eq!(permission_notice(true, true), None);
    }

    #[test]
    fn copy_feedback_expires() {
        let now = Instant::now();
        assert_eq!(copy_label(None, now), "复制地址");
        assert_eq!(copy_label(Some(now), now), "已复制");
        assert_eq!(
            copy_label(Some(now - Duration::from_secs(3)), now),
            "复制地址",
        );
    }

    #[test]
    fn reset_needs_second_click_within_four_seconds() {
        let now = Instant::now();
        assert!(!reset_armed(None, now));
        assert!(reset_armed(Some(now), now));
        assert!(!reset_armed(Some(now - Duration::from_secs(5)), now));
    }

    #[test]
    fn update_check_interval_is_24_hours() {
        assert_eq!(UPDATE_CHECK_INTERVAL, Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn elevated_guide_flag_falls_back_to_user_flag() {
        assert!(guide_already_seen(true, true, false));
        assert!(guide_already_seen(false, true, true));
        assert!(!guide_already_seen(false, true, false));
        assert!(!guide_already_seen(false, false, true));
    }

    #[test]
    fn idle_watch_samples_are_stable_so_the_window_stops_repainting() {
        let state = AppState::new(crate::identity::Identity {
            device_id: "dev-1".into(),
            name: "TestMac".into(),
            secret: "s3cret".into(),
        });
        state.pairing.lock().unwrap().set_window_open(true);
        let updater = crate::updater::Updater::new();
        assert!(Watched::sample(&state, &updater) == Watched::sample(&state, &updater));
    }

    #[test]
    fn layout_uses_one_exact_grid() {
        assert_eq!(CONTENT_W + MARGIN_X * 2.0, WINDOW_W);
        assert_eq!(ADDR_W + TILE_GAP + CODE_W, RIGHT_W);
        assert_eq!(QR_SIDE + COL_GAP + RIGHT_W, CONTENT_W);
        assert_eq!(QR_PAD * 2.0 + QR_IMG, QR_SIDE);
        assert_eq!(QR_IMG, 250.0);
        assert_eq!(BODY_H, QR_SIDE);
        assert_eq!(THEME_W, 114.0);
    }

    #[test]
    fn window_fits_content_but_never_exceeds_the_screen() {
        assert_eq!(fitted_height(812.3, None), 813.0);
        assert_eq!(fitted_height(900.0, None), WINDOW_MAX_H);
        assert_eq!(fitted_height(812.3, Some(1080.0)), 813.0);
        assert_eq!(fitted_height(1200.0, Some(900.0)), 820.0);
        assert_eq!(fitted_height(600.0, Some(300.0)), 320.0_f32.min(600.0));
    }

    #[test]
    fn nic_list_bottom_aligns_with_qr() {
        let above = TILE_H + COL_AFTER_TILE + NIC_TITLE_H + COL_AFTER_TITLE;
        let three = NIC_LIST_PAD_Y * 2.0 + NIC_H * 3.0 + NIC_GAP * 2.0 + NIC_LIST_STROKE * 2.0;
        assert_eq!(nic_list_height(), QR_SIDE - above);
        assert_eq!(nic_list_height(), three);
        assert_eq!(nic_track_h(), NIC_H * 3.0 + NIC_GAP * 2.0);
        assert_eq!(empty_slot_h(0), nic_track_h());
        assert_eq!(empty_slot_h(1), nic_track_h() - NIC_H - NIC_GAP);
        assert_eq!(empty_slot_h(2), nic_track_h() - nic_rows_h(2) - NIC_GAP);
        assert_eq!(empty_slot_h(3), 0.0);
        assert!(nic_rows_h(4) > nic_track_h());
    }

    #[test]
    fn text_and_icons_ink_is_vertically_centered() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        ctx.begin_pass(Default::default());
        for (text, font) in [
            ("诊断日志", egui::FontId::proportional(PROBE_PT)),
            ("地址", egui::FontId::proportional(PROBE_PT)),
            (
                ICON_HELP.codepoint,
                egui::FontId::new(PROBE_PT, ICON_HELP.font_family()),
            ),
            (
                ICON_WIFI.codepoint,
                egui::FontId::new(PROBE_PT, ICON_WIFI.font_family()),
            ),
            (
                ICON_CONTENT_COPY.codepoint,
                egui::FontId::new(PROBE_PT, ICON_CONTENT_COPY.font_family()),
            ),
        ] {
            let off = ink_offset_em(&ctx, text, font);
            assert!(off.abs() < 0.03, "{text}: ink off center by {off:+.3} em");
        }
        apply_app_style(&ctx);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let style = ctx.style_of(theme);
            assert_eq!(style.interaction.tooltip_delay, 0.0);
            assert!(!style.interaction.show_tooltips_only_when_still);
            assert!(style.spacing.tooltip_width < WINDOW_W - 2.0 * MARGIN_X);
            assert_eq!(style.spacing.tooltip_width, TOOLTIP_W);
        }
    }

    #[test]
    fn nic_counter_shows_index_and_total_before_switching() {
        let nics = vec![
            Nic {
                name: "en0".into(),
                ip: "192.168.1.2".into(),
                kind: NicKind::Wifi,
            },
            Nic {
                name: "utun3".into(),
                ip: "100.64.0.2".into(),
                kind: NicKind::Tunnel,
            },
        ];
        assert_eq!(nic_counter(&nics, "100.64.0.2"), "第 2 / 2 张");
        assert_eq!(nic_counter(&nics, "10.0.0.1"), "共 2 张");
        assert_eq!(nic_counter(&[], ""), "共 0 张");
    }
}
