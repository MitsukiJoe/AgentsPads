use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::handle::{self, Conn};
use crate::identity::Identity;
use crate::pairing::{self, Pairing};
use crate::protocol::{InMsg, OutMsg};

const POST_UPDATE_BIND_ATTEMPTS: usize = 50;
const POST_UPDATE_BIND_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const MAX_AUTH_BYTES: usize = 4 * 1024;
const MAX_UNAUTH: usize = 32;
const MAX_UNAUTH_PER_IP: usize = 4;
/// 入站消息和单帧共用。见 `ws_config`。
const WS_MAX_INBOUND: usize = 4 << 20;

/// tokio-tungstenite 0.26 的 `WebSocketStream` 只暴露 `get_config`，没有 `set_config`
/// （内部 `tungstenite::WebSocket::set_config` 够不着）。认证前后只能同一套上限。
/// 4MiB 盖住已有的 1MiB 首包测试和认证后的长文本，同时把默认 64MiB 消息 / 16MiB 帧压下来。
/// 读缓冲从默认 128KiB 降到 8KiB；写缓冲给不读数据的对端一个有限上限。
fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(8 * 1024)
        .write_buffer_size(4 * 1024)
        .max_write_buffer_size(64 * 1024)
        .max_message_size(Some(WS_MAX_INBOUND))
        .max_frame_size(Some(WS_MAX_INBOUND))
}

pub struct AppState {
    /// `identity.secret` 只是启动时的值；当前密钥以 `secret()` 为准。
    pub identity: Identity,
    secret: Mutex<String>,
    pub pairing: Mutex<Pairing>,
    pub paused: Arc<AtomicBool>,
    input_generation: Arc<AtomicU64>,
    next_input_source: AtomicU64,
    pub clients: Mutex<Vec<mpsc::UnboundedSender<OutMsg>>>,
    /// 还没完成首条认证的连接。认证结束就释放，长连接不占名额。
    unauth: Mutex<UnauthSlots>,
}

impl AppState {
    pub fn new(identity: Identity) -> Arc<Self> {
        Arc::new(Self {
            secret: Mutex::new(identity.secret.clone()),
            identity,
            pairing: Mutex::new(Pairing::default()),
            paused: Arc::new(AtomicBool::new(false)),
            input_generation: Arc::new(AtomicU64::new(0)),
            next_input_source: AtomicU64::new(1),
            clients: Mutex::new(Vec::new()),
            unauth: Mutex::new(UnauthSlots::default()),
        })
    }

    /// 未认证名额。满了返回 None，调用方直接丢掉这条 TCP。
    fn try_reserve_unauth(self: &Arc<Self>, ip: IpAddr) -> Option<UnauthGuard> {
        let mut slots = self.unauth.lock().unwrap();
        // ponytail: 未认证只计全局 32 和单 IP 4，不做按秒或按字节的速率限制；名额不够再收紧。
        let ip_count = slots.by_ip.get(&ip).copied().unwrap_or(0);
        if slots.total >= MAX_UNAUTH || ip_count >= MAX_UNAUTH_PER_IP {
            return None;
        }
        slots.total += 1;
        slots.by_ip.insert(ip, ip_count + 1);
        drop(slots);
        Some(UnauthGuard {
            state: Arc::clone(self),
            ip,
        })
    }

    pub fn secret(&self) -> String {
        self.secret.lock().unwrap().clone()
    }

    /// 换新长期密钥并断开所有已配对连接；手机需重新扫码或输入配对码。
    pub fn reset_secret(&self) -> std::io::Result<()> {
        let secret = pairing::random_hex(32);
        crate::identity::save(&Identity {
            secret: secret.clone(),
            ..self.identity.clone()
        })?;
        self.replace_secret(secret);
        Ok(())
    }

    fn replace_secret(&self, secret: String) {
        *self.secret.lock().unwrap() = secret;
        self.clients.lock().unwrap().clear();
    }

    /// 首条文本必须是 hello（HMAC）或 pair（配对码）；成功时 pair 返回要下发的密钥。
    fn authenticate(&self, nonce: &str, first: InMsg) -> Result<Option<String>, &'static str> {
        match first {
            InMsg::Hello { auth, .. } => pairing::verify_auth(&self.secret(), nonce, &auth)
                .then_some(None)
                .ok_or("bad_auth"),
            InMsg::Pair { code, .. } => {
                let ok = self.pairing.lock().unwrap().try_code(&code);
                ok.then(|| Some(self.secret())).ok_or("bad_code")
            }
            _ => Err("unauthenticated"),
        }
    }

    pub fn sync_enabled(&self) -> bool {
        !self.paused.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        let was_paused = self.paused.load(Ordering::SeqCst);
        if paused {
            self.paused.store(true, Ordering::SeqCst);
            if !was_paused {
                self.input_generation.fetch_add(1, Ordering::SeqCst);
            }
            enqueue_pointer_command(PointerCommand::Pause {
                epoch: self.input_epoch(),
            });
        } else if was_paused {
            self.input_generation.fetch_add(1, Ordering::SeqCst);
            self.paused.store(false, Ordering::SeqCst);
        }
        let msg = OutMsg::SyncState {
            sync_enabled: !paused,
        };
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|tx| tx.send(msg.clone()).is_ok());
    }

    fn input_epoch(&self) -> InputEpoch {
        InputEpoch {
            paused: Arc::clone(&self.paused),
            generation: Arc::clone(&self.input_generation),
            observed: self.input_generation.load(Ordering::SeqCst),
        }
    }

    fn next_input_source(&self) -> u64 {
        self.next_input_source.fetch_add(1, Ordering::Relaxed)
    }
}

pub async fn serve(state: Arc<AppState>, bind: SocketAddr) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    crate::logutil::write("connection listen ok");
    tokio::spawn(accept_loop(listener, state));
    Ok(addr)
}

pub async fn serve_with_retry(
    state: Arc<AppState>,
    bind: SocketAddr,
    post_update: bool,
) -> std::io::Result<SocketAddr> {
    let attempts = if post_update {
        POST_UPDATE_BIND_ATTEMPTS
    } else {
        1
    };
    for attempt in 0..attempts {
        match serve(state.clone(), bind).await {
            Err(e)
                if post_update
                    && e.kind() == std::io::ErrorKind::AddrInUse
                    && attempt + 1 < attempts =>
            {
                tokio::time::sleep(POST_UPDATE_BIND_DELAY).await;
            }
            result => return result,
        }
    }
    unreachable!("retry loop always returns")
}

#[derive(Default)]
struct UnauthSlots {
    total: usize,
    by_ip: HashMap<IpAddr, usize>,
}

/// 认证阶段占用的名额。任意退出路径都在 Drop 里归还。
struct UnauthGuard {
    state: Arc<AppState>,
    ip: IpAddr,
}

impl Drop for UnauthGuard {
    fn drop(&mut self) {
        let mut slots = self.state.unauth.lock().unwrap();
        slots.total = slots.total.saturating_sub(1);
        if let Some(count) = slots.by_ip.get_mut(&self.ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                slots.by_ip.remove(&self.ip);
            }
        }
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<AppState>) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let Some(guard) = state.try_reserve_unauth(peer.ip()) else {
                    drop(stream);
                    crate::logutil::write("connection unauth limit");
                    continue;
                };
                crate::logutil::write("connection accept ok");
                let state = state.clone();
                tokio::spawn(async move {
                    if handle_socket(stream, state, guard, AUTH_TIMEOUT)
                        .await
                        .is_err()
                    {
                        crate::logutil::write("connection session failed");
                    }
                });
            }
            Err(_e) => {
                crate::logutil::write("connection accept failed");
            }
        }
    }
}

fn enable_pointer_tcp(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
}

#[derive(Clone)]
struct InputEpoch {
    paused: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    observed: u64,
}

impl InputEpoch {
    fn active(&self) -> bool {
        !self.paused.load(Ordering::SeqCst)
            && self.generation.load(Ordering::SeqCst) == self.observed
    }

    fn same(&self, other: &Self) -> bool {
        self.observed == other.observed && self.same_owner(other)
    }

    fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.paused, &other.paused) && Arc::ptr_eq(&self.generation, &other.generation)
    }
}

enum PointerCommand {
    Actions {
        epoch: InputEpoch,
        source: u64,
        actions: Vec<handle::Action>,
    },
    ReleaseSource {
        epoch: InputEpoch,
        source: u64,
    },
    Pause {
        epoch: InputEpoch,
    },
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(all(windows, feature = "perf-probe")), allow(dead_code))]
pub(crate) struct QueueStamp {
    pub(crate) command_id: u64,
    pub(crate) enqueued_qpc: i64,
}

#[cfg(all(windows, feature = "perf-probe"))]
impl PointerCommand {
    fn queue_kind(&self) -> &'static str {
        match self {
            Self::Actions { .. } => "actions",
            Self::ReleaseSource { .. } => "release_source",
            Self::Pause { .. } => "pause",
        }
    }
}

#[cfg(all(windows, feature = "perf-probe"))]
struct QueueEnvelope {
    command: PointerCommand,
    stamp: Option<QueueStamp>,
}

#[cfg(not(all(windows, feature = "perf-probe")))]
type QueueEnvelope = PointerCommand;

struct QueueWork {
    command: PointerCommand,
    stamp: Option<QueueStamp>,
}

trait IntoQueueWork {
    fn into_queue_work(self) -> QueueWork;
}

impl IntoQueueWork for PointerCommand {
    fn into_queue_work(self) -> QueueWork {
        QueueWork {
            command: self,
            stamp: None,
        }
    }
}

#[cfg(all(windows, feature = "perf-probe"))]
impl IntoQueueWork for QueueEnvelope {
    fn into_queue_work(self) -> QueueWork {
        QueueWork {
            command: self.command,
            stamp: self.stamp,
        }
    }
}

fn enqueue_pointer(actions: Vec<handle::Action>, epoch: InputEpoch, source: u64) {
    enqueue_pointer_command(PointerCommand::Actions {
        epoch,
        source,
        actions,
    });
}

fn enqueue_pointer_command(command: PointerCommand) {
    #[cfg(test)]
    {
        let mut capture = pointer_capture().lock().unwrap();
        if let Some(capture) = capture.as_mut() {
            let epoch = match &command {
                PointerCommand::Actions { epoch, .. }
                | PointerCommand::ReleaseSource { epoch, .. }
                | PointerCommand::Pause { epoch } => epoch,
            };
            let matches_capture = match (&capture.paused, &capture.generation) {
                (Some(paused), Some(generation)) => {
                    Arc::ptr_eq(paused, &epoch.paused) && Arc::ptr_eq(generation, &epoch.generation)
                }
                (None, None) => match &command {
                    PointerCommand::Actions { .. } | PointerCommand::ReleaseSource { .. } => {
                        capture.paused = Some(Arc::clone(&epoch.paused));
                        capture.generation = Some(Arc::clone(&epoch.generation));
                        true
                    }
                    PointerCommand::Pause { .. } => false,
                },
                _ => false,
            };
            if matches_capture {
                let _ = capture.tx.send(command);
                return;
            }
        }
    }
    static TX: std::sync::OnceLock<std::sync::mpsc::Sender<QueueEnvelope>> =
        std::sync::OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<QueueEnvelope>();
        std::thread::Builder::new()
            .name("agentpad-inject".into())
            .spawn(move || run_pointer_queue(rx, handle::apply_actions))
            .expect("inject thread");
        tx
    });
    enqueue_to_pointer_queue(tx, command);
}

#[cfg(all(windows, feature = "perf-probe"))]
fn enqueue_to_pointer_queue(tx: &std::sync::mpsc::Sender<QueueEnvelope>, command: PointerCommand) {
    enqueue_to_pointer_queue_with_probe(tx, command, crate::perf_probe::active());
}

#[cfg(all(windows, feature = "perf-probe"))]
fn enqueue_to_pointer_queue_with_probe(
    tx: &std::sync::mpsc::Sender<QueueEnvelope>,
    command: PointerCommand,
    probe: Option<&crate::perf_probe::Probe>,
) {
    if let Some(probe) = probe {
        let kind = command.queue_kind();
        probe.enqueue_command(kind, |stamp| {
            tx.send(QueueEnvelope {
                command,
                stamp: Some(stamp),
            })
            .is_ok()
        });
    } else {
        let _ = tx.send(QueueEnvelope {
            command,
            stamp: None,
        });
    }
}

#[cfg(not(all(windows, feature = "perf-probe")))]
fn enqueue_to_pointer_queue(tx: &std::sync::mpsc::Sender<QueueEnvelope>, command: PointerCommand) {
    let _ = tx.send(command);
}

#[cfg(test)]
struct PointerCaptureSlot {
    tx: std::sync::mpsc::Sender<PointerCommand>,
    paused: Option<Arc<AtomicBool>>,
    generation: Option<Arc<AtomicU64>>,
}

#[cfg(test)]
fn pointer_capture() -> &'static Mutex<Option<PointerCaptureSlot>> {
    static SLOT: std::sync::OnceLock<Mutex<Option<PointerCaptureSlot>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn run_pointer_queue<T: IntoQueueWork>(
    rx: std::sync::mpsc::Receiver<T>,
    mut apply: impl FnMut(&[handle::Action]),
) {
    #[cfg(all(windows, feature = "perf-probe"))]
    let probe = crate::perf_probe::active();
    #[cfg(not(all(windows, feature = "perf-probe")))]
    let probe = ();
    run_pointer_queue_inner(rx, &mut apply, probe);
}

#[cfg(all(test, windows, feature = "perf-probe"))]
fn run_pointer_queue_with_probe<T: IntoQueueWork>(
    rx: std::sync::mpsc::Receiver<T>,
    mut apply: impl FnMut(&[handle::Action]),
    probe: &crate::perf_probe::Probe,
) {
    run_pointer_queue_inner(rx, &mut apply, Some(probe));
}

fn run_pointer_queue_inner<T: IntoQueueWork>(
    rx: std::sync::mpsc::Receiver<T>,
    apply: &mut impl FnMut(&[handle::Action]),
    #[cfg(all(windows, feature = "perf-probe"))] probe: Option<&crate::perf_probe::Probe>,
    #[cfg(not(all(windows, feature = "perf-probe")))] _probe: (),
) {
    let mut buttons_before_batch = 0;
    let mut buttons_source = None;
    let mut buttons_epoch: Option<InputEpoch> = None;
    let mut pending = None;
    loop {
        let work = match pending.take() {
            Some(work) => work,
            None => match rx.recv() {
                Ok(command) => claim_queue_work(
                    command,
                    #[cfg(all(windows, feature = "perf-probe"))]
                    probe,
                ),
                Err(_) => {
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.queue_closed();
                    }
                    break;
                }
            },
        };
        let QueueWork { command, stamp } = work;
        match command {
            PointerCommand::Pause { epoch } => {
                let held_before_pause = buttons_epoch
                    .as_ref()
                    .is_some_and(|held| held.same_owner(&epoch) && held.observed < epoch.observed);
                #[cfg(all(windows, feature = "perf-probe"))]
                if let (Some(probe), Some(stamp)) = (probe, stamp) {
                    probe.branch_processed(
                        stamp,
                        "pause",
                        if buttons_before_batch != 0 && held_before_pause {
                            "released_held_buttons"
                        } else {
                            "no_held_buttons"
                        },
                    );
                }
                if buttons_before_batch != 0 && held_before_pause {
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.apply_started(
                            stamp,
                            stamp.map(|item| item.command_id),
                            "pause_release",
                            1,
                            stamp.map(|item| item.enqueued_qpc),
                        );
                    }
                    apply(&[released_pointer()]);
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.apply_finished(
                            stamp,
                            stamp.map(|item| item.command_id),
                            "pause_release",
                            1,
                            stamp.map(|item| item.enqueued_qpc),
                        );
                    }
                    buttons_before_batch = 0;
                    buttons_source = None;
                    buttons_epoch = None;
                } else {
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.command_completed(stamp, "pause", "no_held_buttons");
                    }
                }
            }
            PointerCommand::ReleaseSource { epoch, source } => {
                let source_owns_buttons = buttons_epoch
                    .as_ref()
                    .is_some_and(|held| held.same_owner(&epoch));
                let should_release = epoch.active()
                    && buttons_before_batch != 0
                    && buttons_source == Some(source)
                    && source_owns_buttons;
                #[cfg(all(windows, feature = "perf-probe"))]
                if let (Some(probe), Some(stamp)) = (probe, stamp) {
                    probe.branch_processed(
                        stamp,
                        "release_source",
                        if should_release {
                            "released_held_buttons"
                        } else {
                            "no_owned_buttons"
                        },
                    );
                }
                if should_release {
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.apply_started(
                            stamp,
                            stamp.map(|item| item.command_id),
                            "source_release",
                            1,
                            stamp.map(|item| item.enqueued_qpc),
                        );
                    }
                    apply(&[released_pointer()]);
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.apply_finished(
                            stamp,
                            stamp.map(|item| item.command_id),
                            "source_release",
                            1,
                            stamp.map(|item| item.enqueued_qpc),
                        );
                    }
                    buttons_before_batch = 0;
                    buttons_source = None;
                    buttons_epoch = None;
                } else {
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.command_completed(stamp, "release_source", "no_owned_buttons");
                    }
                }
            }
            PointerCommand::Actions {
                epoch,
                source,
                mut actions,
            } => {
                if !epoch.active() {
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let (Some(probe), Some(stamp)) = (probe, stamp) {
                        probe.command_dropped(stamp, "stale_epoch");
                    }
                    continue;
                }
                #[cfg(all(windows, feature = "perf-probe"))]
                let batch_id = stamp.map(|item| item.command_id);
                let mut oldest_enqueue_qpc = stamp.map(|item| item.enqueued_qpc);
                #[cfg(all(windows, feature = "perf-probe"))]
                let mut batch_command_count = 1;
                while let Ok(more) = rx.try_recv() {
                    let more = claim_queue_work(
                        more,
                        #[cfg(all(windows, feature = "perf-probe"))]
                        probe,
                    );
                    let QueueWork {
                        command: more_command,
                        stamp: more_stamp,
                    } = more;
                    match more_command {
                        PointerCommand::Actions {
                            epoch: more_epoch,
                            source: more_source,
                            actions: more_actions,
                        } if source == more_source
                            && epoch.same(&more_epoch)
                            && more_epoch.active() =>
                        {
                            #[cfg(all(windows, feature = "perf-probe"))]
                            {
                                batch_command_count += 1;
                            }
                            if let Some(more_stamp) = more_stamp {
                                oldest_enqueue_qpc = Some(
                                    oldest_enqueue_qpc.map_or(more_stamp.enqueued_qpc, |oldest| {
                                        oldest.min(more_stamp.enqueued_qpc)
                                    }),
                                );
                                #[cfg(all(windows, feature = "perf-probe"))]
                                if let (Some(probe), Some(batch_id)) = (probe, batch_id) {
                                    probe.command_merged(more_stamp, batch_id, batch_command_count);
                                }
                            }
                            merge_pointer_batch(&mut actions, more_actions, buttons_before_batch);
                        }
                        other => {
                            #[cfg(all(windows, feature = "perf-probe"))]
                            if let (Some(probe), Some(more_stamp)) = (probe, more_stamp) {
                                probe.command_pending(more_stamp, "lookahead_barrier");
                            }
                            pending = Some(QueueWork {
                                command: other,
                                stamp: more_stamp,
                            });
                            break;
                        }
                    }
                }
                if !epoch.active() {
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if let Some(probe) = probe {
                        probe.batch_dropped(
                            stamp,
                            batch_id,
                            batch_command_count,
                            "stale_epoch_during_lookahead",
                            oldest_enqueue_qpc,
                        );
                    }
                    continue;
                }
                #[cfg(all(windows, feature = "perf-probe"))]
                if let Some(probe) = probe {
                    probe.apply_started(
                        stamp,
                        batch_id,
                        "actions",
                        batch_command_count,
                        oldest_enqueue_qpc,
                    );
                }
                apply(&actions);
                #[cfg(all(windows, feature = "perf-probe"))]
                if let Some(probe) = probe {
                    probe.apply_finished(
                        stamp,
                        batch_id,
                        "actions",
                        batch_command_count,
                        oldest_enqueue_qpc,
                    );
                }
                if let Some(handle::Action::Pointer { buttons, .. }) = actions.last() {
                    buttons_before_batch = *buttons;
                    buttons_source = (*buttons != 0).then_some(source);
                    buttons_epoch = (*buttons != 0).then(|| epoch.clone());
                }
            }
        }
    }
}

fn claim_queue_work<T: IntoQueueWork>(
    command: T,
    #[cfg(all(windows, feature = "perf-probe"))] probe: Option<&crate::perf_probe::Probe>,
) -> QueueWork {
    let work = command.into_queue_work();
    #[cfg(all(windows, feature = "perf-probe"))]
    if let (Some(probe), Some(stamp)) = (probe, work.stamp) {
        probe.command_claimed(stamp);
    }
    work
}

fn released_pointer() -> handle::Action {
    handle::Action::Pointer {
        dx: 0.0,
        dy: 0.0,
        buttons: 0,
        wheel: 0,
    }
}

fn merge_pointer_batch(
    dst: &mut Vec<handle::Action>,
    more: Vec<handle::Action>,
    buttons_before_batch: u8,
) {
    for action in more {
        let buttons_before_tail = match &dst[..] {
            [.., handle::Action::Pointer { buttons, .. }, _] => Some(*buttons),
            [_] => Some(buttons_before_batch),
            _ => None,
        };
        match (&mut dst[..], action) {
            (
                [.., handle::Action::Pointer {
                    dx,
                    dy,
                    buttons,
                    wheel,
                }],
                handle::Action::Pointer {
                    dx: ddx,
                    dy: ddy,
                    buttons: btn,
                    wheel: wh,
                },
                // Native injection moves before changing buttons, so an edge packet
                // must not absorb motion or wheel input that happened after the edge.
            ) if *buttons == btn && buttons_before_tail == Some(btn) => {
                *dx += ddx;
                *dy += ddy;
                *wheel += wh;
            }
            (_, action) => dst.push(action),
        }
    }
}

async fn read_first_text(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> Option<String> {
    while let Some(Ok(frame)) = ws.next().await {
        match frame {
            Message::Text(text) => return Some(text.to_string()),
            Message::Close(_) => return None,
            _ => {}
        }
    }
    None
}

async fn handle_socket(
    stream: TcpStream,
    state: Arc<AppState>,
    unauth: UnauthGuard,
    auth_timeout: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    enable_pointer_tcp(&stream);
    // 从 TCP 接入到首条认证消息读完，共用同一个截止时间。超时丢掉整个 future，连接一起关。
    let opened = tokio::time::timeout(auth_timeout, async {
        let mut ws = tokio_tungstenite::accept_async_with_config(stream, Some(ws_config())).await?;
        let nonce = pairing::random_hex(16);
        let challenge = OutMsg::Challenge {
            nonce: nonce.clone(),
            device_id: state.identity.device_id.clone(),
        };
        ws.send(Message::Text(serde_json::to_string(&challenge)?.into()))
            .await?;
        let first = read_first_text(&mut ws).await;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((ws, nonce, first))
    })
    .await;
    let (mut ws, nonce, first) = match opened {
        Ok(Ok(ready)) => ready,
        Ok(Err(err)) => return Err(err),
        Err(_elapsed) => {
            crate::logutil::write("connection auth timeout");
            return Ok(());
        }
    };
    let first = first
        .filter(|text| text.len() <= MAX_AUTH_BYTES)
        .and_then(|text| serde_json::from_str::<InMsg>(&text).ok());
    let (tx, mut rx) = mpsc::unbounded_channel::<OutMsg>();
    let verdict = first.map_or(Err("unauthenticated"), |msg| {
        let mut clients = state.clients.lock().unwrap();
        let verdict = state.authenticate(&nonce, msg);
        if verdict.is_ok() {
            clients.retain(|t| !t.is_closed());
            clients.push(tx);
        }
        verdict
    });
    let secret = match verdict {
        Ok(secret) => {
            drop(unauth);
            secret
        }
        Err(reason) => {
            crate::logutil::write("connection auth failed");
            let failed = OutMsg::AuthFailed { reason };
            let _ = ws
                .send(Message::Text(serde_json::to_string(&failed)?.into()))
                .await;
            let _ = ws.close(None).await;
            return Ok(());
        }
    };
    crate::logutil::write(if secret.is_some() {
        "connection pair ok"
    } else {
        "connection auth ok"
    });
    let ips: Vec<String> = crate::net::list_nics().into_iter().map(|n| n.ip).collect();
    let connected = OutMsg::Connected {
        device_id: state.identity.device_id.clone(),
        name: state.identity.name.clone(),
        os: agentpad_input::os().to_string(),
        sync_enabled: state.sync_enabled(),
        ips,
        secret,
    };
    ws.send(Message::Text(serde_json::to_string(&connected)?.into()))
        .await?;

    let (mut sink, mut source) = ws.split();
    let mut conn = Conn::default();
    let input_source = state.next_input_source();
    // 内层 future 吸收读写上的 `?`，无论 break、出错还是被重置密钥踢掉，外层都先抬起按键。
    let session = async {
        loop {
            tokio::select! {
                out = rx.recv() => {
                    let Some(msg) = out else { break; };
                    sink.send(Message::Text(serde_json::to_string(&msg)?.into())).await?;
                    crate::logutil::write("message send ok");
                }
                incoming = source.next() => {
                    let Some(frame) = incoming else { break; };
                    let Message::Text(text) = frame? else { continue; };
                    let Ok(msg) = serde_json::from_str::<InMsg>(&text) else {
                        crate::logutil::write("message parse failed");
                        continue;
                    };
                    let pointer = matches!(msg, InMsg::Pointer { .. });
                    #[cfg(all(windows, feature = "perf-probe"))]
                    if pointer {
                        if let Some(probe) = crate::perf_probe::active() {
                            probe.phone_pointer_received();
                        }
                    }
                    let epoch = state.input_epoch();
                    let paused = state.paused.load(Ordering::SeqCst);
                    crate::logutil::operation(crate::logutil::input_category(&msg), false, !paused || matches!(msg, InMsg::Hello { .. } | InMsg::Ping));
                    let (replies, actions) = handle::handle(paused, state.sync_enabled(), &mut conn, msg);
                    if !actions.is_empty() {
                        if actions
                            .iter()
                            .all(|a| matches!(a, handle::Action::Pointer { .. }))
                        {
                            if !paused {
                                enqueue_pointer(actions, epoch, input_source);
                            }
                        } else {
                            tokio::task::block_in_place(|| handle::apply_actions(&actions));
                        }
                    }
                    for r in replies {
                        sink.send(Message::Text(serde_json::to_string(&r)?.into())).await?;
                        if !pointer {
                            crate::logutil::write("message send ok");
                        }
                    }
                }
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;
    if handle::release_stuck_pointer(&mut conn).is_some() {
        enqueue_pointer_command(PointerCommand::ReleaseSource {
            epoch: state.input_epoch(),
            source: input_source,
        });
    }
    session?;
    crate::logutil::write("connection session closed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use futures_util::StreamExt;
    use tokio_tungstenite::connect_async;

    #[test]
    fn merges_backlogged_pointer_batches() {
        let mut batch = vec![handle::Action::Pointer {
            dx: 1.0,
            dy: 2.0,
            buttons: 0,
            wheel: 0,
        }];
        merge_pointer_batch(
            &mut batch,
            vec![handle::Action::Pointer {
                dx: 3.0,
                dy: -1.0,
                buttons: 0,
                wheel: 4,
            }],
            0,
        );
        assert_eq!(
            batch,
            vec![handle::Action::Pointer {
                dx: 4.0,
                dy: 1.0,
                buttons: 0,
                wheel: 4,
            }]
        );
    }

    #[test]
    fn keeps_click_button_edges_when_merging_backlog() {
        let mut batch = vec![handle::Action::Pointer {
            dx: 0.0,
            dy: 0.0,
            buttons: 1,
            wheel: 0,
        }];
        merge_pointer_batch(
            &mut batch,
            vec![handle::Action::Pointer {
                dx: 0.0,
                dy: 0.0,
                buttons: 0,
                wheel: 0,
            }],
            0,
        );
        assert_eq!(
            batch,
            vec![
                handle::Action::Pointer {
                    dx: 0.0,
                    dy: 0.0,
                    buttons: 1,
                    wheel: 0,
                },
                handle::Action::Pointer {
                    dx: 0.0,
                    dy: 0.0,
                    buttons: 0,
                    wheel: 0,
                },
            ]
        );
    }

    fn pointer(dx: f64, buttons: u8, wheel: i32) -> handle::Action {
        handle::Action::Pointer {
            dx,
            dy: 0.0,
            buttons,
            wheel,
        }
    }

    #[test]
    fn keeps_drag_start_before_backlogged_motion() {
        let mut batch = vec![pointer(0.0, 1, 0)];
        merge_pointer_batch(&mut batch, vec![pointer(4.0, 1, 0)], 0);
        assert_eq!(batch, vec![pointer(0.0, 1, 0), pointer(4.0, 1, 0)]);
    }

    #[test]
    fn keeps_release_before_backlogged_hover() {
        let mut batch = vec![pointer(0.0, 1, 0), pointer(0.0, 0, 0)];
        merge_pointer_batch(&mut batch, vec![pointer(4.0, 0, 0)], 0);
        assert_eq!(
            batch,
            vec![pointer(0.0, 1, 0), pointer(0.0, 0, 0), pointer(4.0, 0, 0)]
        );
    }

    #[test]
    fn keeps_wheel_after_backlogged_button_edge() {
        let mut batch = vec![pointer(0.0, 1, 0)];
        merge_pointer_batch(&mut batch, vec![pointer(0.0, 1, 3)], 0);
        assert_eq!(batch, vec![pointer(0.0, 1, 0), pointer(0.0, 1, 3)]);
    }

    #[test]
    fn still_merges_backlogged_drag_after_button_edge() {
        let mut batch = vec![pointer(0.0, 1, 0), pointer(1.0, 1, 1)];
        merge_pointer_batch(&mut batch, vec![pointer(2.0, 1, 2), pointer(3.0, 1, 3)], 0);
        assert_eq!(batch, vec![pointer(0.0, 1, 0), pointer(6.0, 1, 6)]);
    }

    #[test]
    fn keeps_release_edge_at_start_of_next_batch() {
        let mut batch = vec![pointer(0.0, 0, 0)];
        merge_pointer_batch(&mut batch, vec![pointer(4.0, 0, 0)], 1);
        assert_eq!(batch, vec![pointer(0.0, 0, 0), pointer(4.0, 0, 0)]);
    }

    #[test]
    fn merges_held_drag_across_batches() {
        let mut batch = vec![pointer(1.0, 1, 1)];
        merge_pointer_batch(&mut batch, vec![pointer(2.0, 1, 2)], 1);
        assert_eq!(batch, vec![pointer(3.0, 1, 3)]);
    }

    #[test]
    fn pointer_worker_keeps_edges_after_stalled_injection() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut first = true;
            run_pointer_queue(rx, |batch| {
                observed_tx.send(batch.to_vec()).unwrap();
                if first {
                    first = false;
                    resume_rx.recv().unwrap();
                }
            });
        });
        let timeout = std::time::Duration::from_secs(5);
        let epoch = test_epoch();
        tx.send(actions_command(&epoch, 1, vec![pointer(0.0, 1, 0)]))
            .unwrap();
        assert_eq!(
            observed_rx.recv_timeout(timeout).unwrap(),
            vec![pointer(0.0, 1, 0)]
        );
        tx.send(actions_command(&epoch, 1, vec![pointer(0.0, 0, 0)]))
            .unwrap();
        tx.send(actions_command(&epoch, 1, vec![pointer(4.0, 0, 1)]))
            .unwrap();
        tx.send(actions_command(&epoch, 1, vec![pointer(2.0, 0, 2)]))
            .unwrap();
        drop(tx);
        resume_tx.send(()).unwrap();
        assert_eq!(
            observed_rx.recv_timeout(timeout).unwrap(),
            vec![pointer(0.0, 0, 0), pointer(6.0, 0, 3)]
        );
        worker.join().unwrap();
    }

    #[test]
    fn pause_releases_once_and_drops_queued_downs_from_every_connection() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut applied_count = 0;
            run_pointer_queue(rx, |batch| {
                observed_tx.send(batch.to_vec()).unwrap();
                applied_count += 1;
                if applied_count == 2 {
                    resume_rx.recv().unwrap();
                }
            });
        });
        let timeout = std::time::Duration::from_secs(5);
        let epoch = test_epoch();
        let first_down = pointer(1.0, 1, 0);
        let second_down = pointer(2.0, 2, 0);
        tx.send(actions_command(&epoch, 1, vec![first_down.clone()]))
            .unwrap();
        assert_eq!(
            observed_rx.recv_timeout(timeout).unwrap(),
            vec![first_down.clone()]
        );
        tx.send(actions_command(&epoch, 2, vec![second_down.clone()]))
            .unwrap();
        assert_eq!(
            observed_rx.recv_timeout(timeout).unwrap(),
            vec![second_down.clone()]
        );

        tx.send(actions_command(&epoch, 3, vec![pointer(3.0, 1, 0)]))
            .unwrap();
        epoch.paused.store(true, Ordering::SeqCst);
        epoch.generation.fetch_add(1, Ordering::SeqCst);
        let pause_epoch = InputEpoch {
            observed: 1,
            ..epoch.clone()
        };
        tx.send(PointerCommand::Pause {
            epoch: pause_epoch.clone(),
        })
        .unwrap();
        tx.send(PointerCommand::Pause { epoch: pause_epoch })
            .unwrap();
        epoch.generation.fetch_add(1, Ordering::SeqCst);
        epoch.paused.store(false, Ordering::SeqCst);
        let resumed = InputEpoch {
            observed: 2,
            ..epoch.clone()
        };
        let resumed_down = pointer(4.0, 1, 0);
        tx.send(actions_command(&resumed, 2, vec![resumed_down.clone()]))
            .unwrap();
        drop(tx);
        resume_tx.send(()).unwrap();

        let mut applied = vec![first_down, second_down];
        while let Ok(batch) = observed_rx.recv_timeout(timeout) {
            applied.extend(batch);
        }
        worker.join().unwrap();
        assert_eq!(
            applied,
            vec![
                pointer(1.0, 1, 0),
                pointer(2.0, 2, 0),
                button_up(),
                resumed_down,
            ]
        );
    }

    #[test]
    fn pause_only_releases_buttons_owned_by_its_app_state() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_pointer_queue(rx, |batch| {
                observed_tx.send(batch.to_vec()).unwrap();
            });
        });
        let timeout = std::time::Duration::from_secs(5);
        let held_epoch = test_epoch();
        let first_down = pointer(1.0, 1, 0);
        tx.send(actions_command(&held_epoch, 7, vec![first_down.clone()]))
            .unwrap();
        assert_eq!(observed_rx.recv_timeout(timeout).unwrap(), vec![first_down]);

        let unrelated = test_epoch();
        unrelated.generation.fetch_add(1, Ordering::SeqCst);
        tx.send(PointerCommand::Pause { epoch: unrelated }).unwrap();
        assert!(observed_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());

        held_epoch.generation.fetch_add(1, Ordering::SeqCst);
        let matching_pause = InputEpoch {
            observed: 1,
            ..held_epoch
        };
        tx.send(PointerCommand::Pause {
            epoch: matching_pause,
        })
        .unwrap();
        assert_eq!(
            observed_rx.recv_timeout(timeout).unwrap(),
            vec![button_up()]
        );
        drop(tx);
        worker.join().unwrap();
    }

    #[test]
    fn disconnect_release_only_releases_the_connection_that_holds_buttons() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_pointer_queue(rx, |batch| {
                observed_tx.send(batch.to_vec()).unwrap();
            });
        });
        let timeout = std::time::Duration::from_secs(5);
        let epoch = test_epoch();
        let first_down = pointer(1.0, 1, 0);
        let second_down = pointer(2.0, 2, 0);
        tx.send(actions_command(&epoch, 1, vec![first_down.clone()]))
            .unwrap();
        assert_eq!(observed_rx.recv_timeout(timeout).unwrap(), vec![first_down]);
        tx.send(actions_command(&epoch, 2, vec![second_down.clone()]))
            .unwrap();
        assert_eq!(
            observed_rx.recv_timeout(timeout).unwrap(),
            vec![second_down]
        );

        tx.send(PointerCommand::ReleaseSource {
            epoch: epoch.clone(),
            source: 1,
        })
        .unwrap();
        assert!(observed_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
        tx.send(PointerCommand::ReleaseSource { epoch, source: 2 })
            .unwrap();
        assert_eq!(
            observed_rx.recv_timeout(timeout).unwrap(),
            vec![button_up()]
        );
        drop(tx);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn ordinary_second_instance_does_not_retry() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();
        let state = AppState::new(test_identity());
        let err = serve_with_retry(state, addr, false).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[tokio::test]
    async fn post_update_waits_for_previous_listener() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(held);
        });
        let state = AppState::new(test_identity());
        let bound = serve_with_retry(state, addr, true).await.unwrap();
        release.join().unwrap();
        assert_eq!(bound, addr);
    }

    fn test_identity() -> Identity {
        Identity {
            device_id: "dev-1".into(),
            name: "TestMac".into(),
            secret: "s3cret".into(),
        }
    }

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn next_json(ws: &mut Client) -> Option<serde_json::Value> {
        loop {
            match ws.next().await? {
                Ok(Message::Text(text)) => return Some(serde_json::from_str(&text).unwrap()),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    /// 连接并以首条消息 `first(nonce)` 回应挑战，返回连接与服务端回复。
    async fn open(
        addr: SocketAddr,
        first: impl FnOnce(&str) -> String,
    ) -> (Client, Option<serde_json::Value>) {
        let (mut ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let challenge = next_json(&mut ws).await.unwrap();
        assert_eq!(challenge["type"], "challenge");
        assert_eq!(challenge["device_id"], "dev-1");
        let nonce = challenge["nonce"].as_str().unwrap().to_string();
        assert_eq!(nonce.len(), 32);
        ws.send(Message::Text(first(&nonce).into())).await.unwrap();
        let reply = next_json(&mut ws).await;
        (ws, reply)
    }

    fn hello(secret: &str, nonce: &str) -> String {
        format!(
            r#"{{"type":"hello","client_id":"c","client_name":"Android","auth":"{}"}}"#,
            pairing::auth_tag(secret, nonce)
        )
    }

    fn pair(code: &str) -> String {
        format!(r#"{{"type":"pair","client_id":"c","client_name":"Android","code":"{code}"}}"#)
    }

    #[tokio::test]
    async fn unauthenticated_clients_are_rejected_before_any_input() {
        let state = AppState::new(test_identity());
        let addr = serve(state.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        for (first, reason) in [
            (
                r#"{"type":"text","content":"x","auto_enter":true,"send_mode":"submit"}"#
                    .to_string(),
                "unauthenticated",
            ),
            (r#"{"type":"ping"}"#.to_string(), "unauthenticated"),
            (hello("wrong", "n"), "bad_auth"),
            (pair("0000"), "bad_code"),
        ] {
            let (mut ws, reply) = open(addr, |_| first).await;
            let reply = reply.unwrap();
            assert_eq!(reply["type"], "auth_failed");
            assert_eq!(reply["reason"], reason);
            assert!(next_json(&mut ws).await.is_none());
        }
        assert!(state.clients.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn authentication_first_message_has_4kib_limit() {
        let state = AppState::new(test_identity());
        let addr = serve(state, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        for size in [4096, 4097, 1 << 20] {
            let (mut ws, reply) = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                open(addr, |nonce| {
                    let mut text = hello("s3cret", nonce);
                    text.push_str(&" ".repeat(size - text.len()));
                    text
                }),
            )
            .await
            .unwrap();
            let reply = reply.unwrap();
            if size == 4096 {
                assert_eq!(reply["type"], "connected");
            } else {
                assert_eq!(reply["type"], "auth_failed", "size {size}");
                assert_eq!(reply["reason"], "unauthenticated");
                assert!(next_json(&mut ws).await.is_none());
            }
        }
    }

    #[tokio::test]
    async fn authenticated_long_text_is_not_subject_to_auth_limit() {
        let state = AppState::new(test_identity());
        state.set_paused(true);
        let addr = serve(state, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let (mut ws, _connected) = open(addr, |nonce| hello("s3cret", nonce)).await;
        let text = serde_json::json!({
            "type": "text",
            "content": "文".repeat(32_768),
            "auto_enter": false,
            "send_mode": "submit",
        })
        .to_string();
        assert!(text.len() > 4096);
        ws.send(Message::Text(text.into())).await.unwrap();
        let ack = tokio::time::timeout(std::time::Duration::from_secs(5), next_json(&mut ws))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ack["type"], "ack");
        assert_eq!(ack["ok"], false);
        ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
            .await
            .unwrap();
        assert_eq!(next_json(&mut ws).await.unwrap()["type"], "pong");
    }

    #[tokio::test]
    async fn secret_reset_rejects_pending_old_key_authentication() {
        let state = AppState::new(test_identity());
        let addr = serve(state.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let (mut pending, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let challenge = next_json(&mut pending).await.unwrap();
        let nonce = challenge["nonce"].as_str().unwrap();
        state.replace_secret("new-secret".into());
        pending
            .send(Message::Text(hello("s3cret", nonce).into()))
            .await
            .unwrap();
        let failed = next_json(&mut pending).await.unwrap();
        assert_eq!(failed["type"], "auth_failed");
        assert_eq!(failed["reason"], "bad_auth");
        assert!(next_json(&mut pending).await.is_none());
        let (_ws, reply) = open(addr, |nonce| hello("new-secret", nonce)).await;
        assert_eq!(reply.unwrap()["type"], "connected");
    }

    #[tokio::test]
    async fn pairing_code_hands_out_secret_once_and_reset_disconnects() {
        let state = AppState::new(test_identity());
        let addr = serve(state.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        state.pairing.lock().unwrap().set_window_open(true);
        let code = state.pairing.lock().unwrap().code().unwrap().to_string();
        let (_ws, reply) = open(addr, |_| pair(&code)).await;
        let reply = reply.unwrap();
        assert_eq!(reply["type"], "connected");
        assert_eq!(reply["secret"], "s3cret");

        let (_, reply) = open(addr, |_| pair(&code)).await;
        assert_eq!(reply.unwrap()["reason"], "bad_code");

        let (mut paired, reply) = open(addr, |n| hello("s3cret", n)).await;
        let reply = reply.unwrap();
        assert_eq!(reply["type"], "connected");
        assert!(reply.get("secret").is_none());

        state.replace_secret(pairing::random_hex(32));
        assert!(next_json(&mut paired).await.is_none());
        let (_, reply) = open(addr, |n| hello("s3cret", n)).await;
        assert_eq!(reply.unwrap()["reason"], "bad_auth");
        let secret = state.secret();
        let (_, reply) = open(addr, |n| hello(&secret, n)).await;
        assert_eq!(reply.unwrap()["type"], "connected");
    }

    #[tokio::test]
    async fn connect_receives_connected() {
        let state = AppState::new(test_identity());
        let addr = serve(state, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let (_ws, v) = open(addr, |n| hello("s3cret", n)).await;
        let v = v.unwrap();
        assert_eq!(v["type"], "connected");
        assert_eq!(v["device_id"], "dev-1");
        assert_eq!(v["name"], "TestMac");
        assert_eq!(v["os"], agentpad_input::os());
        assert_eq!(v["sync_enabled"], true);
    }

    #[tokio::test]
    async fn ping_pong_and_paused_ack() {
        let state = AppState::new(test_identity());
        state.set_paused(true);
        let addr = serve(state, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let (mut ws, _connected) = open(addr, |n| hello("s3cret", n)).await;
        ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
            .await
            .unwrap();
        let pong = ws.next().await.unwrap().unwrap();
        let Message::Text(text) = pong else { panic!() };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "pong");
        assert_eq!(v["sync_enabled"], false);

        ws.send(Message::Text(
            r#"{"type":"text","content":"x","auto_enter":false,"send_mode":"submit"}"#.into(),
        ))
        .await
        .unwrap();
        let ack = ws.next().await.unwrap().unwrap();
        let Message::Text(text) = ack else { panic!() };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn unauth_slots_cap_per_ip_and_global_then_release() {
        let state = AppState::new(test_identity());
        let ip_a = IpAddr::from([192, 0, 2, 1]);
        let mut held = Vec::new();
        for _ in 0..MAX_UNAUTH_PER_IP {
            held.push(state.try_reserve_unauth(ip_a).unwrap());
        }
        assert!(state.try_reserve_unauth(ip_a).is_none());
        let ip_b = IpAddr::from([192, 0, 2, 2]);
        held.push(state.try_reserve_unauth(ip_b).unwrap());

        let mut rest = Vec::new();
        for n in 0..(MAX_UNAUTH - held.len()) {
            let ip = IpAddr::from([198, 51, 100, (n + 1) as u8]);
            rest.push(state.try_reserve_unauth(ip).unwrap());
        }
        assert_eq!(state.unauth.lock().unwrap().total, MAX_UNAUTH);
        assert!(state
            .try_reserve_unauth(IpAddr::from([203, 0, 113, 1]))
            .is_none());

        held.remove(0);
        assert!(state.try_reserve_unauth(ip_a).is_some());
        rest.pop();
        assert!(state
            .try_reserve_unauth(IpAddr::from([203, 0, 113, 2]))
            .is_some());
        drop(held);
        drop(rest);
        assert_eq!(state.unauth.lock().unwrap().total, 0);
        assert!(state.unauth.lock().unwrap().by_ip.is_empty());
    }

    async fn wait_unauth(state: &AppState, total: usize) {
        let wait = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while state.unauth.lock().unwrap().total != total {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            wait.is_ok(),
            "unauth total stayed {}",
            state.unauth.lock().unwrap().total
        );
    }

    async fn tcp_closed_within(stream: &mut TcpStream, limit: std::time::Duration) -> bool {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 8];
        matches!(
            tokio::time::timeout(limit, stream.read(&mut buf)).await,
            Ok(Ok(0)) | Ok(Err(_))
        )
    }

    #[tokio::test]
    async fn unauth_cap_rejects_extra_tcp_and_releases_when_it_ends() {
        let state = AppState::new(test_identity());
        let addr = serve(state.clone(), "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        let (mut bad, reply) = open(addr, |_| r#"{"type":"ping"}"#.to_string()).await;
        assert_eq!(reply.unwrap()["reason"], "unauthenticated");
        assert!(next_json(&mut bad).await.is_none());
        wait_unauth(&state, 0).await;

        let (mut authed, reply) = open(addr, |n| hello("s3cret", n)).await;
        assert_eq!(reply.unwrap()["type"], "connected");
        assert_eq!(state.unauth.lock().unwrap().total, 0);

        let mut hangs = Vec::new();
        for _ in 0..MAX_UNAUTH_PER_IP {
            hangs.push(TcpStream::connect(addr).await.unwrap());
        }
        wait_unauth(&state, MAX_UNAUTH_PER_IP).await;

        let mut extra = TcpStream::connect(addr).await.unwrap();
        assert!(tcp_closed_within(&mut extra, std::time::Duration::from_secs(2)).await);
        assert_eq!(state.unauth.lock().unwrap().total, MAX_UNAUTH_PER_IP);

        drop(hangs);
        wait_unauth(&state, 0).await;
        authed
            .send(Message::Text(r#"{"type":"ping"}"#.into()))
            .await
            .unwrap();
        assert_eq!(next_json(&mut authed).await.unwrap()["type"], "pong");
    }

    #[tokio::test]
    async fn stalled_handshake_closes_at_injected_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState::new(test_identity());
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let guard = server_state.try_reserve_unauth(peer.ip()).unwrap();
            let started = std::time::Instant::now();
            let result = super::handle_socket(
                stream,
                server_state.clone(),
                guard,
                std::time::Duration::from_millis(300),
            )
            .await;
            (
                started.elapsed(),
                result,
                server_state.unauth.lock().unwrap().total,
            )
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        assert!(!tcp_closed_within(&mut client, std::time::Duration::from_millis(80)).await);
        assert!(tcp_closed_within(&mut client, std::time::Duration::from_secs(2)).await);
        let (elapsed, result, left) =
            tokio::time::timeout(std::time::Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap();
        assert!(result.is_ok());
        assert_eq!(left, 0);
        assert!(elapsed < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn silent_client_after_challenge_closes_at_same_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState::new(test_identity());
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let guard = server_state.try_reserve_unauth(peer.ip()).unwrap();
            super::handle_socket(
                stream,
                server_state,
                guard,
                std::time::Duration::from_millis(800),
            )
            .await
        });
        let (mut ws, _) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            connect_async(format!("ws://{addr}")),
        )
        .await
        .unwrap()
        .unwrap();
        let challenge = tokio::time::timeout(std::time::Duration::from_secs(2), next_json(&mut ws))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(challenge["type"], "challenge");
        let early =
            tokio::time::timeout(std::time::Duration::from_millis(150), next_json(&mut ws)).await;
        assert!(early.is_err(), "closed before the deadline");
        let later = tokio::time::timeout(std::time::Duration::from_secs(2), next_json(&mut ws))
            .await
            .unwrap();
        assert!(later.is_none());
        assert!(server.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_the_body() {
        use tokio::io::AsyncWriteExt;
        let state = AppState::new(test_identity());
        let addr = serve(state, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let (mut ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let challenge = next_json(&mut ws).await.unwrap();
        assert_eq!(challenge["type"], "challenge");
        let mut header = [0u8; 14];
        header[0] = 0x81;
        header[1] = 0xFF;
        header[2..10].copy_from_slice(&(WS_MAX_INBOUND as u64 + 1).to_be_bytes());
        {
            let tokio_tungstenite::MaybeTlsStream::Plain(tcp) = ws.get_mut() else {
                panic!("expected plain tcp");
            };
            tcp.write_all(&header).await.unwrap();
            tcp.flush().await.unwrap();
        }
        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), next_json(&mut ws))
            .await
            .expect("oversized frame was not rejected");
        match reply {
            None => {}
            Some(value) => {
                assert_eq!(value["type"], "auth_failed");
                assert_eq!(value["reason"], "unauthenticated");
            }
        }
    }

    #[tokio::test]
    async fn accepted_socket_disables_nagle() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            super::enable_pointer_tcp(&stream);
            stream.nodelay().unwrap()
        });
        let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
        assert!(server.await.unwrap());
    }

    fn button_up() -> handle::Action {
        handle::Action::Pointer {
            dx: 0.0,
            dy: 0.0,
            buttons: 0,
            wheel: 0,
        }
    }

    fn test_epoch() -> InputEpoch {
        InputEpoch {
            paused: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            observed: 0,
        }
    }

    fn actions_command(
        epoch: &InputEpoch,
        source: u64,
        actions: Vec<handle::Action>,
    ) -> PointerCommand {
        PointerCommand::Actions {
            epoch: epoch.clone(),
            source,
            actions,
        }
    }

    #[cfg(all(windows, feature = "perf-probe"))]
    #[test]
    fn pointer_probe_counts_concurrent_producers_without_queue_races() {
        let probe = Arc::new(crate::perf_probe::Probe::for_test(32_768, 32));
        let (tx, rx) = std::sync::mpsc::channel::<QueueEnvelope>();
        let worker_probe = Arc::clone(&probe);
        let worker = std::thread::spawn(move || {
            run_pointer_queue_with_probe(rx, |_| {}, &worker_probe);
        });

        let producer_count = 4;
        let commands_per_producer = 250;
        let producers = (0..producer_count)
            .map(|producer| {
                let tx = tx.clone();
                let probe = Arc::clone(&probe);
                std::thread::spawn(move || {
                    let epoch = test_epoch();
                    for _ in 0..commands_per_producer {
                        enqueue_to_pointer_queue_with_probe(
                            &tx,
                            PointerCommand::Pause {
                                epoch: epoch.clone(),
                            },
                            Some(&probe),
                        );
                    }
                    producer
                })
            })
            .collect::<Vec<_>>();

        for (expected, producer) in producers.into_iter().enumerate() {
            assert_eq!(producer.join().unwrap(), expected);
        }
        drop(tx);
        worker.join().unwrap();

        let counters = probe.counters_for_test();
        let expected = (producer_count * commands_per_producer) as u64;
        assert_eq!(counters.enqueued, expected);
        assert_eq!(counters.pause_enqueued, expected);
        assert_eq!(counters.claimed, expected);
        assert_eq!(counters.pause_processed, expected);
        assert_eq!(counters.current_queue_depth, 0);
        assert_eq!(counters.current_in_flight, 0);
        assert_eq!(counters.counter_underflow, 0);
        assert_eq!(counters.queue_closed, 1);
    }

    #[cfg(all(windows, feature = "perf-probe"))]
    #[test]
    fn pointer_probe_counts_merge_pending_release_and_apply_boundaries() {
        let probe = crate::perf_probe::Probe::for_test(128, 32);
        let (tx, rx) = std::sync::mpsc::channel::<QueueEnvelope>();
        let epoch = test_epoch();
        enqueue_to_pointer_queue_with_probe(
            &tx,
            actions_command(&epoch, 17, vec![pointer(0.0, 1, 0)]),
            Some(&probe),
        );
        enqueue_to_pointer_queue_with_probe(
            &tx,
            actions_command(&epoch, 17, vec![pointer(1.0, 1, 0)]),
            Some(&probe),
        );
        enqueue_to_pointer_queue_with_probe(
            &tx,
            actions_command(&epoch, 17, vec![pointer(2.0, 1, 0)]),
            Some(&probe),
        );
        enqueue_to_pointer_queue_with_probe(
            &tx,
            PointerCommand::ReleaseSource {
                epoch: epoch.clone(),
                source: 17,
            },
            Some(&probe),
        );

        let (applied_tx, applied_rx) = std::sync::mpsc::channel();
        let worker_probe = Arc::new(probe);
        let probe_for_worker = Arc::clone(&worker_probe);
        let worker = std::thread::spawn(move || {
            run_pointer_queue_with_probe(
                rx,
                |batch| applied_tx.send(batch.to_vec()).unwrap(),
                &probe_for_worker,
            );
        });
        drop(tx);

        assert_eq!(
            applied_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            vec![pointer(0.0, 1, 0), pointer(3.0, 1, 0)]
        );
        assert_eq!(
            applied_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            vec![button_up()]
        );
        worker.join().unwrap();

        let counters = worker_probe.counters_for_test();
        assert_eq!(counters.enqueued, 4);
        assert_eq!(counters.claimed, 4);
        assert_eq!(counters.merged, 2);
        assert_eq!(counters.pending, 1);
        assert_eq!(counters.release_processed, 1);
        assert_eq!(counters.applied_commands, 4);
        assert_eq!(counters.current_queue_depth, 0);
        assert_eq!(counters.current_in_flight, 0);
        assert_eq!(counters.counter_underflow, 0);
        let event_names = worker_probe.event_names_for_test();
        for event in [
            "pointer_command_enqueued",
            "pointer_command_claimed",
            "pointer_command_merged",
            "pointer_command_pending",
            "pointer_apply_start",
            "pointer_apply_end",
            "pointer_release_source_processed",
            "pointer_queue_closed",
        ] {
            assert!(event_names.contains(&event), "missing {event}");
        }
    }

    #[cfg(all(windows, feature = "perf-probe"))]
    #[test]
    fn pointer_probe_counts_stale_drop_pause_and_each_failed_send_kind() {
        let probe = crate::perf_probe::Probe::for_test(128, 32);
        let (tx, rx) = std::sync::mpsc::channel::<QueueEnvelope>();
        let stale_epoch = test_epoch();
        let active_epoch = InputEpoch {
            observed: 1,
            ..stale_epoch.clone()
        };
        stale_epoch.generation.fetch_add(1, Ordering::SeqCst);
        enqueue_to_pointer_queue_with_probe(
            &tx,
            actions_command(&stale_epoch, 3, vec![pointer(5.0, 1, 0)]),
            Some(&probe),
        );
        enqueue_to_pointer_queue_with_probe(
            &tx,
            PointerCommand::Pause {
                epoch: active_epoch,
            },
            Some(&probe),
        );
        let worker_probe = Arc::new(probe);
        let probe_for_worker = Arc::clone(&worker_probe);
        let worker = std::thread::spawn(move || {
            run_pointer_queue_with_probe(rx, |_| {}, &probe_for_worker);
        });
        drop(tx);
        worker.join().unwrap();

        let counters = worker_probe.counters_for_test();
        assert_eq!(counters.dropped, 1);
        assert_eq!(counters.pause_processed, 1);
        assert_eq!(counters.claimed, 2);
        assert_eq!(counters.current_queue_depth, 0);
        assert_eq!(counters.current_in_flight, 0);
        assert_eq!(counters.counter_underflow, 0);
        assert_eq!(counters.queue_closed, 1);

        let failed_probe = crate::perf_probe::Probe::for_test(32, 8);
        let (failed_tx, failed_rx) = std::sync::mpsc::channel::<QueueEnvelope>();
        drop(failed_rx);
        let epoch = test_epoch();
        enqueue_to_pointer_queue_with_probe(
            &failed_tx,
            actions_command(&epoch, 4, vec![pointer(1.0, 0, 0)]),
            Some(&failed_probe),
        );
        enqueue_to_pointer_queue_with_probe(
            &failed_tx,
            PointerCommand::ReleaseSource {
                epoch: epoch.clone(),
                source: 4,
            },
            Some(&failed_probe),
        );
        enqueue_to_pointer_queue_with_probe(
            &failed_tx,
            PointerCommand::Pause { epoch },
            Some(&failed_probe),
        );
        let failed = failed_probe.counters_for_test();
        assert_eq!(failed.send_failed, 3);
        assert_eq!(failed.action_send_failed, 1);
        assert_eq!(failed.release_send_failed, 1);
        assert_eq!(failed.pause_send_failed, 1);
        assert_eq!(failed.current_queue_depth, 0);
        assert_eq!(failed.current_in_flight, 0);
        assert_eq!(failed.counter_underflow, 0);
    }

    fn pointer_json(dx: f64, dy: f64, buttons: u8, wheel: i32) -> String {
        format!(r#"{{"type":"pointer","dx":{dx},"dy":{dy},"buttons":{buttons},"wheel":{wheel}}}"#)
    }

    /// 把指针动作拦在注入线程外面，测试里不会碰到真的键鼠。
    struct PointerCapture {
        rx: std::sync::mpsc::Receiver<PointerCommand>,
        _gate: tokio::sync::MutexGuard<'static, ()>,
    }

    impl PointerCapture {
        async fn install() -> Self {
            static GATE: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
            let gate = GATE
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock()
                .await;
            let (tx, rx) = std::sync::mpsc::channel();
            *super::pointer_capture().lock().unwrap() = Some(super::PointerCaptureSlot {
                tx,
                paused: None,
                generation: None,
            });
            Self { rx, _gate: gate }
        }

        fn bind_state(&self, state: &AppState) {
            let mut capture = super::pointer_capture().lock().unwrap();
            if let Some(capture) = capture.as_mut() {
                capture.paused = Some(Arc::clone(&state.paused));
                capture.generation = Some(Arc::clone(&state.input_generation));
            }
        }

        async fn recv_command(&self) -> PointerCommand {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match self.rx.try_recv() {
                    Ok(command) => return command,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        panic!("pointer capture closed")
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        if tokio::time::Instant::now() >= deadline {
                            panic!("timed out waiting for pointer action");
                        }
                        tokio::task::yield_now().await;
                    }
                }
            }
        }

        async fn recv(&self) -> Vec<handle::Action> {
            match self.recv_command().await {
                PointerCommand::Actions { actions, .. } => actions,
                PointerCommand::ReleaseSource { .. } => vec![button_up()],
                PointerCommand::Pause { .. } => {
                    panic!("pause control is not an injected action")
                }
            }
        }

        /// 这段时间里让出运行时，确认没有再入队。阻塞等待会冻住当前线程上的服务端任务。
        async fn quiet(&self, limit: std::time::Duration) -> bool {
            let start = tokio::time::Instant::now();
            while start.elapsed() < limit {
                if self.rx.try_recv().is_ok() {
                    return false;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            self.rx.try_recv().is_err()
        }
    }

    impl Drop for PointerCapture {
        fn drop(&mut self) {
            *super::pointer_capture().lock().unwrap() = None;
        }
    }

    async fn authed_client(state: Arc<AppState>) -> (SocketAddr, Client) {
        let addr = serve(state, "127.0.0.1:0".parse().unwrap()).await.unwrap();
        let (ws, reply) = open(addr, |n| hello("s3cret", n)).await;
        assert_eq!(reply.unwrap()["type"], "connected");
        (addr, ws)
    }

    #[tokio::test]
    async fn disconnect_after_press_enqueues_one_release() {
        let capture = PointerCapture::install().await;
        let state = AppState::new(test_identity());
        let (_addr, mut ws) = authed_client(state).await;
        ws.send(Message::Text(pointer_json(1.5, -2.0, 1, 0).into()))
            .await
            .unwrap();
        assert_eq!(
            capture.recv().await,
            vec![handle::Action::Pointer {
                dx: 1.5,
                dy: -2.0,
                buttons: 1,
                wheel: 0,
            }]
        );
        drop(ws);
        assert_eq!(capture.recv().await, vec![button_up()]);
        assert!(capture.quiet(std::time::Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn button_up_then_disconnect_does_not_release_again() {
        let capture = PointerCapture::install().await;
        let state = AppState::new(test_identity());
        let (_addr, mut ws) = authed_client(state).await;
        ws.send(Message::Text(pointer_json(1.0, 0.0, 1, 0).into()))
            .await
            .unwrap();
        assert_eq!(capture.recv().await, vec![pointer(1.0, 1, 0)]);
        ws.send(Message::Text(pointer_json(3.0, 4.0, 0, 0).into()))
            .await
            .unwrap();
        assert_eq!(
            capture.recv().await,
            vec![handle::Action::Pointer {
                dx: 3.0,
                dy: 4.0,
                buttons: 0,
                wheel: 0,
            }]
        );
        drop(ws);
        assert!(capture.quiet(std::time::Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn disconnect_without_press_enqueues_nothing() {
        let capture = PointerCapture::install().await;
        let state = AppState::new(test_identity());
        let (_addr, ws) = authed_client(state).await;
        drop(ws);
        assert!(capture.quiet(std::time::Duration::from_millis(300)).await);
    }

    #[tokio::test]
    async fn pause_releases_button_immediately_and_ignores_later_remote_up() {
        let capture = PointerCapture::install().await;
        let state = AppState::new(test_identity());
        let (_addr, mut ws) = authed_client(state.clone()).await;
        ws.send(Message::Text(pointer_json(1.0, 0.0, 1, 0).into()))
            .await
            .unwrap();
        assert_eq!(capture.recv().await, vec![pointer(1.0, 1, 0)]);
        state.set_paused(true);
        assert!(matches!(
            capture.recv_command().await,
            PointerCommand::Pause { .. }
        ));
        ws.send(Message::Text(pointer_json(8.0, 9.0, 1, 2).into()))
            .await
            .unwrap();
        assert!(capture.quiet(std::time::Duration::from_millis(200)).await);
        ws.send(Message::Text(pointer_json(8.0, 9.0, 0, 4).into()))
            .await
            .unwrap();
        assert!(capture.quiet(std::time::Duration::from_millis(200)).await);
        drop(ws);
        assert!(capture.quiet(std::time::Duration::from_millis(200)).await);
    }

    #[tokio::test]
    async fn secret_reset_and_protocol_error_still_release() {
        use tokio::io::AsyncWriteExt;
        let capture = PointerCapture::install().await;
        let state = AppState::new(test_identity());
        let (_addr, mut ws) = authed_client(state.clone()).await;
        ws.send(Message::Text(pointer_json(2.0, 0.0, 1, 0).into()))
            .await
            .unwrap();
        assert_eq!(capture.recv().await, vec![pointer(2.0, 1, 0)]);
        state.replace_secret("rotated-secret".into());
        assert_eq!(capture.recv().await, vec![button_up()]);

        let second_state = AppState::new(test_identity());
        capture.bind_state(&second_state);
        let (_addr, mut ws) = authed_client(second_state).await;
        ws.send(Message::Text(pointer_json(2.0, 0.0, 5, 0).into()))
            .await
            .unwrap();
        assert_eq!(capture.recv().await, vec![pointer(2.0, 5, 0)]);
        {
            let tokio_tungstenite::MaybeTlsStream::Plain(tcp) = ws.get_mut() else {
                panic!("expected plain tcp");
            };
            // 孤立的 continuation，服务端读帧会出错，走 `?` 退出。
            tcp.write_all(&[0x80, 0x80, 0, 0, 0, 0]).await.unwrap();
            tcp.flush().await.unwrap();
        }
        assert_eq!(capture.recv().await, vec![button_up()]);
    }

    #[test]
    fn queued_release_reaches_fake_apply_intact() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_pointer_queue(rx, |batch| {
                seen_tx.send(batch.to_vec()).unwrap();
            });
        });
        let epoch = test_epoch();
        let down = pointer(4.0, 1, 0);
        let up = button_up();
        tx.send(actions_command(&epoch, 1, vec![down.clone()]))
            .unwrap();
        tx.send(actions_command(&epoch, 1, vec![up.clone()]))
            .unwrap();
        drop(tx);
        let mut applied = Vec::new();
        while let Ok(batch) = seen_rx.recv_timeout(std::time::Duration::from_secs(2)) {
            applied.extend(batch);
        }
        worker.join().unwrap();
        assert_eq!(applied, vec![down, up]);
    }
}
