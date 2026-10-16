use crate::ws::QueueStamp;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

const EVENT_CAPACITY: usize = 65_536;
const CRITICAL_EVENT_CAPACITY: usize = 32;
const QPC_PLACEHOLDER: &[u8] = b"-9223372036854775808";
const CRITICAL_EVENTS: [&str; 7] = [
    "tray_exit_callback",
    "poll_tray_exit",
    "stop_input_done",
    "gui_returned",
    "before_process_exit",
    "trace_snapshot_begin",
    "trace_write_failed",
];

static PROBE: OnceLock<Option<Probe>> = OnceLock::new();

#[derive(Clone, Debug, Serialize)]
struct Event {
    seq: u64,
    qpc: i64,
    event: &'static str,
    queue_depth: u64,
    in_flight: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_command_count: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct QueueCounters {
    pub(crate) enqueued: u64,
    pub(crate) actions_enqueued: u64,
    pub(crate) release_enqueued: u64,
    pub(crate) pause_enqueued: u64,
    pub(crate) send_failed: u64,
    pub(crate) action_send_failed: u64,
    pub(crate) release_send_failed: u64,
    pub(crate) pause_send_failed: u64,
    pub(crate) claimed: u64,
    pub(crate) merged: u64,
    pub(crate) pending: u64,
    pub(crate) dropped: u64,
    pub(crate) applied_commands: u64,
    pub(crate) release_processed: u64,
    pub(crate) pause_processed: u64,
    pub(crate) queue_closed: u64,
    pub(crate) counter_underflow: u64,
    pub(crate) max_queue_depth: u64,
    pub(crate) max_in_flight: u64,
    pub(crate) max_wait_ticks: i64,
    pub(crate) max_batch_ticks: i64,
    pub(crate) current_queue_depth: u64,
    pub(crate) current_in_flight: u64,
}

#[derive(Default)]
struct Recorder {
    next_seq: u64,
    next_command_id: u64,
    ordinary_events: VecDeque<Event>,
    critical_events: VecDeque<Event>,
    event_counts: BTreeMap<&'static str, u64>,
    queue: QueueCounters,
    dropped_events: u64,
    dropped_critical_events: u64,
}

#[derive(Serialize)]
struct TraceDocument {
    schema_version: u32,
    clock_source: &'static str,
    queue_scope: &'static str,
    scope_notes: &'static str,
    opt_in: &'static str,
    integrity_level: &'static str,
    qpc_frequency: i64,
    qpc_anchor: i64,
    qpc_anchor_sampling_span_ticks: i64,
    qpc_resolution_ticks: u32,
    utc_anchor_unix_ns: u64,
    pid: u32,
    session_id: u32,
    host_process_created_filetime_100ns: Option<u64>,
    run_id: String,
    snapshot_started_qpc: i64,
    snapshot_serialization_finished_qpc: i64,
    snapshot_bulk_write_flush_finished_qpc: i64,
    snapshot_footer_patch_write_excluded: bool,
    snapshot_write_boundary: &'static str,
    queue_counters: QueueCounters,
    event_counts: BTreeMap<&'static str, u64>,
    dropped_events: u64,
    dropped_critical_events: u64,
    events: Vec<Event>,
}

pub(crate) struct Probe {
    qpc_frequency: i64,
    qpc_anchor: i64,
    qpc_anchor_sampling_span_ticks: i64,
    utc_anchor_unix_ns: u64,
    pid: u32,
    session_id: u32,
    host_process_created_filetime_100ns: Option<u64>,
    run_id: String,
    ordinary_event_capacity: usize,
    critical_event_capacity: usize,
    recorder: Mutex<Recorder>,
    output: Mutex<Option<File>>,
    finalized: AtomicBool,
}

pub(crate) fn initialize() -> bool {
    if crate::elevation::is_elevated() {
        return false;
    }
    PROBE
        .get_or_init(|| Probe::from_environment().ok())
        .is_some()
}

pub(crate) fn active() -> Option<&'static Probe> {
    PROBE.get().and_then(Option::as_ref)
}

impl Probe {
    fn from_environment() -> std::io::Result<Self> {
        let Some(path) = std::env::var_os("AGENTPAD_PERF_TRACE") else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "trace output was not requested",
            ));
        };
        let path = PathBuf::from(path);
        let output_path = validate_output_path(
            &path,
            &crate::identity::data_dir(),
            crate::elevation::protected_state_dir().as_deref(),
            protected_install_root().as_deref(),
        )
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "trace path rejected")
        })?;
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)?;
        let (qpc_frequency, qpc_start) = qpc_frequency_and_counter()?;
        let utc_anchor_unix_ns = unix_time_ns()?;
        let qpc_end = qpc_counter()?;
        let qpc_anchor = qpc_start + (qpc_end - qpc_start) / 2;
        let mut session_id = 0;
        unsafe {
            let _ = ProcessIdToSessionId(std::process::id(), &mut session_id);
        }
        Ok(Self {
            qpc_frequency,
            qpc_anchor,
            qpc_anchor_sampling_span_ticks: qpc_end.saturating_sub(qpc_start),
            utc_anchor_unix_ns,
            pid: std::process::id(),
            session_id,
            host_process_created_filetime_100ns: process_created_filetime_100ns(),
            run_id: Uuid::new_v4().to_string(),
            ordinary_event_capacity: EVENT_CAPACITY - CRITICAL_EVENT_CAPACITY,
            critical_event_capacity: CRITICAL_EVENT_CAPACITY,
            recorder: Mutex::new(Recorder::default()),
            output: Mutex::new(Some(output)),
            finalized: AtomicBool::new(false),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(event_capacity: usize, critical_capacity: usize) -> Self {
        let (qpc_frequency, qpc_start) = qpc_frequency_and_counter().unwrap();
        let utc_anchor_unix_ns = unix_time_ns().unwrap();
        let qpc_end = qpc_counter().unwrap();
        let mut session_id = 0;
        unsafe {
            let _ = ProcessIdToSessionId(std::process::id(), &mut session_id);
        }
        Self {
            qpc_frequency,
            qpc_anchor: qpc_start + (qpc_end - qpc_start) / 2,
            qpc_anchor_sampling_span_ticks: qpc_end.saturating_sub(qpc_start),
            utc_anchor_unix_ns,
            pid: std::process::id(),
            session_id,
            host_process_created_filetime_100ns: process_created_filetime_100ns(),
            run_id: Uuid::new_v4().to_string(),
            ordinary_event_capacity: event_capacity,
            critical_event_capacity: critical_capacity,
            recorder: Mutex::new(Recorder::default()),
            output: Mutex::new(None),
            finalized: AtomicBool::new(false),
        }
    }

    pub(crate) fn enqueue_command(
        &self,
        kind: &'static str,
        send: impl FnOnce(QueueStamp) -> bool,
    ) -> bool {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.next_command_id = recorder.next_command_id.saturating_add(1);
        let stamp = QueueStamp {
            command_id: recorder.next_command_id,
            enqueued_qpc: qpc_counter().unwrap_or(0),
        };
        if send(stamp) {
            recorder.queue.enqueued = recorder.queue.enqueued.saturating_add(1);
            match kind {
                "actions" => {
                    recorder.queue.actions_enqueued =
                        recorder.queue.actions_enqueued.saturating_add(1)
                }
                "release_source" => {
                    recorder.queue.release_enqueued =
                        recorder.queue.release_enqueued.saturating_add(1)
                }
                "pause" => {
                    recorder.queue.pause_enqueued = recorder.queue.pause_enqueued.saturating_add(1)
                }
                _ => {}
            }
            recorder.queue.current_queue_depth =
                recorder.queue.current_queue_depth.saturating_add(1);
            recorder.queue.max_queue_depth = recorder
                .queue
                .max_queue_depth
                .max(recorder.queue.current_queue_depth);
            self.record_locked(
                &mut recorder,
                stamp.enqueued_qpc,
                "pointer_command_enqueued",
                Some(stamp.command_id),
                None,
                Some(kind),
                None,
                None,
                None,
            );
            true
        } else {
            recorder.queue.send_failed = recorder.queue.send_failed.saturating_add(1);
            match kind {
                "actions" => {
                    recorder.queue.action_send_failed =
                        recorder.queue.action_send_failed.saturating_add(1)
                }
                "release_source" => {
                    recorder.queue.release_send_failed =
                        recorder.queue.release_send_failed.saturating_add(1)
                }
                "pause" => {
                    recorder.queue.pause_send_failed =
                        recorder.queue.pause_send_failed.saturating_add(1)
                }
                _ => {}
            }
            self.record_locked(
                &mut recorder,
                qpc_counter().unwrap_or(stamp.enqueued_qpc),
                "pointer_send_failed",
                Some(stamp.command_id),
                None,
                Some(kind),
                Some("channel_closed"),
                None,
                None,
            );
            false
        }
    }

    pub(crate) fn phone_pointer_received(&self) {
        self.record("phone_pointer_received", None, None, None, None, None);
    }

    pub(crate) fn record(
        &self,
        event: &'static str,
        source: Option<&'static str>,
        outcome: Option<&'static str>,
        command_kind: Option<&'static str>,
        command_id: Option<u64>,
        batch_id: Option<u64>,
    ) {
        let mut recorder = self.recorder.lock().unwrap();
        self.record_locked(
            &mut recorder,
            qpc_counter().unwrap_or(0),
            event,
            command_id,
            batch_id,
            command_kind.or(source),
            outcome,
            None,
            None,
        );
    }

    pub(crate) fn command_claimed(&self, stamp: QueueStamp) {
        let mut recorder = self.recorder.lock().unwrap();
        let now = qpc_counter().unwrap_or(stamp.enqueued_qpc);
        let queue_underflow = recorder.queue.current_queue_depth == 0;
        if queue_underflow {
            recorder.queue.counter_underflow = recorder.queue.counter_underflow.saturating_add(1);
        } else {
            recorder.queue.current_queue_depth -= 1;
        }
        recorder.queue.current_in_flight = recorder.queue.current_in_flight.saturating_add(1);
        recorder.queue.claimed = recorder.queue.claimed.saturating_add(1);
        recorder.queue.max_in_flight = recorder
            .queue
            .max_in_flight
            .max(recorder.queue.current_in_flight);
        let wait_ticks = now.saturating_sub(stamp.enqueued_qpc);
        recorder.queue.max_wait_ticks = recorder.queue.max_wait_ticks.max(wait_ticks);
        if queue_underflow {
            self.record_locked(
                &mut recorder,
                now,
                "pointer_queue_counter_underflow",
                Some(stamp.command_id),
                None,
                Some("queue_depth"),
                None,
                None,
                None,
            );
        }
        self.record_locked(
            &mut recorder,
            now,
            "pointer_command_claimed",
            Some(stamp.command_id),
            None,
            None,
            None,
            Some(wait_ticks),
            None,
        );
    }

    pub(crate) fn command_merged(
        &self,
        stamp: QueueStamp,
        batch_id: u64,
        batch_command_count: u64,
    ) {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.queue.merged = recorder.queue.merged.saturating_add(1);
        self.record_locked(
            &mut recorder,
            qpc_counter().unwrap_or(stamp.enqueued_qpc),
            "pointer_command_merged",
            Some(stamp.command_id),
            Some(batch_id),
            None,
            None,
            None,
            Some(batch_command_count),
        );
    }

    pub(crate) fn command_pending(&self, stamp: QueueStamp, reason: &'static str) {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.queue.pending = recorder.queue.pending.saturating_add(1);
        self.record_locked(
            &mut recorder,
            qpc_counter().unwrap_or(stamp.enqueued_qpc),
            "pointer_command_pending",
            Some(stamp.command_id),
            None,
            None,
            Some(reason),
            None,
            None,
        );
    }

    pub(crate) fn branch_processed(
        &self,
        stamp: QueueStamp,
        kind: &'static str,
        outcome: &'static str,
    ) {
        let mut recorder = self.recorder.lock().unwrap();
        match kind {
            "pause" => {
                recorder.queue.pause_processed = recorder.queue.pause_processed.saturating_add(1)
            }
            "release_source" => {
                recorder.queue.release_processed =
                    recorder.queue.release_processed.saturating_add(1)
            }
            _ => {}
        }
        let event = if kind == "pause" {
            "pointer_pause_processed"
        } else {
            "pointer_release_source_processed"
        };
        self.record_locked(
            &mut recorder,
            qpc_counter().unwrap_or(stamp.enqueued_qpc),
            event,
            Some(stamp.command_id),
            None,
            Some(kind),
            Some(outcome),
            None,
            None,
        );
    }

    pub(crate) fn apply_started(
        &self,
        stamp: Option<QueueStamp>,
        batch_id: Option<u64>,
        kind: &'static str,
        command_count: u64,
        oldest_enqueue_qpc: Option<i64>,
    ) {
        let mut recorder = self.recorder.lock().unwrap();
        let now = qpc_counter().unwrap_or(0);
        let wait_ticks = oldest_enqueue_qpc.map(|oldest| now.saturating_sub(oldest));
        self.record_locked(
            &mut recorder,
            now,
            "pointer_apply_start",
            stamp.map(|item| item.command_id),
            batch_id,
            Some(kind),
            None,
            wait_ticks,
            Some(command_count),
        );
    }

    pub(crate) fn apply_finished(
        &self,
        stamp: Option<QueueStamp>,
        batch_id: Option<u64>,
        kind: &'static str,
        command_count: u64,
        oldest_enqueue_qpc: Option<i64>,
    ) {
        let mut recorder = self.recorder.lock().unwrap();
        let now = qpc_counter().unwrap_or(0);
        let wait_ticks = oldest_enqueue_qpc.map(|oldest| now.saturating_sub(oldest));
        recorder.queue.applied_commands = recorder
            .queue
            .applied_commands
            .saturating_add(command_count);
        self.decrement_in_flight(&mut recorder, command_count);
        if let Some(wait_ticks) = wait_ticks {
            recorder.queue.max_batch_ticks = recorder.queue.max_batch_ticks.max(wait_ticks);
        }
        self.record_locked(
            &mut recorder,
            now,
            "pointer_apply_end",
            stamp.map(|item| item.command_id),
            batch_id,
            Some(kind),
            None,
            wait_ticks,
            Some(command_count),
        );
    }

    pub(crate) fn command_dropped(&self, stamp: QueueStamp, reason: &'static str) {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.queue.dropped = recorder.queue.dropped.saturating_add(1);
        self.decrement_in_flight(&mut recorder, 1);
        let now = qpc_counter().unwrap_or(stamp.enqueued_qpc);
        self.record_locked(
            &mut recorder,
            now,
            "pointer_command_dropped",
            Some(stamp.command_id),
            None,
            None,
            Some(reason),
            Some(now.saturating_sub(stamp.enqueued_qpc)),
            None,
        );
    }

    pub(crate) fn batch_dropped(
        &self,
        stamp: Option<QueueStamp>,
        batch_id: Option<u64>,
        command_count: u64,
        reason: &'static str,
        oldest_enqueue_qpc: Option<i64>,
    ) {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.queue.dropped = recorder.queue.dropped.saturating_add(command_count);
        self.decrement_in_flight(&mut recorder, command_count);
        let now = qpc_counter().unwrap_or(0);
        self.record_locked(
            &mut recorder,
            now,
            "pointer_batch_dropped",
            stamp.map(|item| item.command_id),
            batch_id,
            None,
            Some(reason),
            oldest_enqueue_qpc.map(|oldest| now.saturating_sub(oldest)),
            Some(command_count),
        );
    }

    pub(crate) fn command_completed(
        &self,
        stamp: Option<QueueStamp>,
        kind: &'static str,
        outcome: &'static str,
    ) {
        let mut recorder = self.recorder.lock().unwrap();
        self.decrement_in_flight(&mut recorder, 1);
        self.record_locked(
            &mut recorder,
            qpc_counter().unwrap_or(0),
            "pointer_command_completed",
            stamp.map(|item| item.command_id),
            None,
            Some(kind),
            Some(outcome),
            None,
            Some(1),
        );
    }

    pub(crate) fn queue_closed(&self) {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.queue.queue_closed = recorder.queue.queue_closed.saturating_add(1);
        self.record_locked(
            &mut recorder,
            qpc_counter().unwrap_or(0),
            "pointer_queue_closed",
            None,
            None,
            None,
            None,
            None,
            None,
        );
    }

    pub(crate) fn finalize(&self) -> std::io::Result<()> {
        if self.finalized.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let snapshot_started_qpc = qpc_counter().unwrap_or(0);
        self.record("trace_snapshot_begin", None, None, None, None, None);
        let mut bytes = self.snapshot_bytes(snapshot_started_qpc)?;
        let serialization_finished_qpc = qpc_counter().unwrap_or(0);
        let _ = replace_qpc_placeholder(&mut bytes, serialization_finished_qpc)?;
        let write_complete_offset = find_qpc_placeholder(&bytes, 0).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "snapshot QPC marker missing",
            )
        })?;
        let mut output = self.output.lock().unwrap();
        let Some(file) = output.as_mut() else {
            return Ok(());
        };
        file.write_all(&bytes)?;
        file.flush()?;
        let bulk_write_flush_finished_qpc = qpc_counter().unwrap_or(0);
        file.seek(SeekFrom::Start(write_complete_offset as u64))?;
        file.write_all(&qpc_field_bytes(bulk_write_flush_finished_qpc)?)?;
        file.flush()?;
        Ok(())
    }

    fn snapshot_bytes(&self, snapshot_started_qpc: i64) -> std::io::Result<Vec<u8>> {
        let recorder = self.recorder.lock().unwrap();
        let mut events =
            Vec::with_capacity(recorder.ordinary_events.len() + recorder.critical_events.len());
        events.extend(recorder.ordinary_events.iter().cloned());
        events.extend(recorder.critical_events.iter().cloned());
        events.sort_unstable_by_key(|event| event.seq);
        let document = TraceDocument {
            schema_version: 1,
            clock_source: "windows_qpc",
            queue_scope: "desktop_pointer_injection_mpsc",
            scope_notes: "Only the desktop pointer-injection std::sync::mpsc queue is instrumented; Tokio outbound OutMsg channels are excluded.",
            opt_in: "Cargo feature perf-probe plus AGENTPAD_PERF_TRACE absolute file path",
            integrity_level: "unelevated",
            qpc_frequency: self.qpc_frequency,
            qpc_anchor: self.qpc_anchor,
            qpc_anchor_sampling_span_ticks: self.qpc_anchor_sampling_span_ticks,
            qpc_resolution_ticks: 1,
            utc_anchor_unix_ns: self.utc_anchor_unix_ns,
            pid: self.pid,
            session_id: self.session_id,
            host_process_created_filetime_100ns: self.host_process_created_filetime_100ns,
            run_id: self.run_id.clone(),
            snapshot_started_qpc,
            snapshot_serialization_finished_qpc: i64::MIN,
            snapshot_bulk_write_flush_finished_qpc: i64::MIN,
            snapshot_footer_patch_write_excluded: true,
            snapshot_write_boundary: "after File::write_all and flush; no durable-storage sync; fixed-width QPC footer patch excluded",
            queue_counters: recorder.queue.clone(),
            event_counts: recorder.event_counts.clone(),
            dropped_events: recorder.dropped_events,
            dropped_critical_events: recorder.dropped_critical_events,
            events,
        };
        serde_json::to_vec_pretty(&document)
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    fn decrement_in_flight(&self, recorder: &mut Recorder, count: u64) {
        if recorder.queue.current_in_flight < count {
            recorder.queue.counter_underflow = recorder.queue.counter_underflow.saturating_add(1);
            recorder.queue.current_in_flight = 0;
            self.record_locked(
                recorder,
                qpc_counter().unwrap_or(0),
                "pointer_queue_counter_underflow",
                None,
                None,
                Some("in_flight"),
                None,
                None,
                Some(count),
            );
        } else {
            recorder.queue.current_in_flight -= count;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_locked(
        &self,
        recorder: &mut Recorder,
        qpc: i64,
        event: &'static str,
        command_id: Option<u64>,
        batch_id: Option<u64>,
        command_kind: Option<&'static str>,
        outcome: Option<&'static str>,
        wait_ticks: Option<i64>,
        batch_command_count: Option<u64>,
    ) {
        recorder.next_seq = recorder.next_seq.saturating_add(1);
        let count = recorder.event_counts.entry(event).or_default();
        *count = count.saturating_add(1);
        let row = Event {
            seq: recorder.next_seq,
            qpc,
            event,
            queue_depth: recorder.queue.current_queue_depth,
            in_flight: recorder.queue.current_in_flight,
            command_id,
            batch_id,
            context: command_kind,
            outcome,
            wait_ticks,
            batch_command_count,
        };
        if CRITICAL_EVENTS.contains(&event) {
            if recorder.critical_events.len() == self.critical_event_capacity {
                recorder.critical_events.pop_front();
                recorder.dropped_critical_events =
                    recorder.dropped_critical_events.saturating_add(1);
            }
            recorder.critical_events.push_back(row);
        } else if recorder.ordinary_events.len() < self.ordinary_event_capacity {
            recorder.ordinary_events.push_back(row);
        } else {
            recorder.dropped_events = recorder.dropped_events.saturating_add(1);
        }
    }

    #[cfg(test)]
    pub(crate) fn counters_for_test(&self) -> QueueCounters {
        self.recorder.lock().unwrap().queue.clone()
    }

    #[cfg(test)]
    pub(crate) fn event_names_for_test(&self) -> Vec<&'static str> {
        let recorder = self.recorder.lock().unwrap();
        recorder
            .ordinary_events
            .iter()
            .chain(recorder.critical_events.iter())
            .map(|event| event.event)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn dropped_events_for_test(&self) -> u64 {
        self.recorder.lock().unwrap().dropped_events
    }
}

fn find_qpc_placeholder(bytes: &[u8], start: usize) -> Option<usize> {
    bytes
        .get(start..)?
        .windows(QPC_PLACEHOLDER.len())
        .position(|window| window == QPC_PLACEHOLDER)
        .map(|offset| start + offset)
}

fn qpc_field_bytes(value: i64) -> std::io::Result<[u8; QPC_PLACEHOLDER.len()]> {
    let value = value.to_string();
    if value.len() > QPC_PLACEHOLDER.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "QPC value does not fit snapshot timing field",
        ));
    }
    let mut bytes = [b' '; QPC_PLACEHOLDER.len()];
    bytes[..value.len()].copy_from_slice(value.as_bytes());
    Ok(bytes)
}

fn replace_qpc_placeholder(bytes: &mut [u8], value: i64) -> std::io::Result<usize> {
    let offset = find_qpc_placeholder(bytes, 0).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "snapshot QPC marker missing",
        )
    })?;
    let field_end = offset + QPC_PLACEHOLDER.len();
    bytes[offset..field_end].copy_from_slice(&qpc_field_bytes(value)?);
    Ok(offset)
}

fn qpc_frequency_and_counter() -> std::io::Result<(i64, i64)> {
    let mut frequency = 0;
    let mut counter = 0;
    unsafe {
        QueryPerformanceFrequency(&mut frequency)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        QueryPerformanceCounter(&mut counter)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    Ok((frequency, counter))
}

fn qpc_counter() -> std::io::Result<i64> {
    let mut counter = 0;
    unsafe {
        QueryPerformanceCounter(&mut counter)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    Ok(counter)
}

fn unix_time_ns() -> std::io::Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    u64::try_from(duration.as_nanos()).map_err(|error| std::io::Error::other(error.to_string()))
}

fn process_created_filetime_100ns() -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
        .ok()?;
    }
    Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

fn protected_install_root() -> Option<PathBuf> {
    crate::elevation::protected_state_dir().and_then(|path| path.parent().map(Path::to_path_buf))
}

fn validate_output_path(
    path: &Path,
    identity_dir: &Path,
    protected_state_dir: Option<&Path>,
    protected_install_root: Option<&Path>,
) -> Option<PathBuf> {
    if !path.is_absolute() || path.file_name().is_none() || path.exists() {
        return None;
    }
    let parent = path.parent()?.canonicalize().ok()?;
    let output_path = parent.join(path.file_name()?);
    let identity_dir = normalize_guard_path(identity_dir)?;
    if path_is_within(&output_path, &identity_dir) {
        return None;
    }
    for protected in [protected_state_dir, protected_install_root]
        .into_iter()
        .flatten()
    {
        let protected = normalize_guard_path(protected)?;
        if path_is_within(&output_path, &protected) {
            return None;
        }
    }
    Some(output_path)
}

fn normalize_guard_path(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        return path.canonicalize().ok();
    }
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(ancestor.file_name()?.to_os_string());
        ancestor = ancestor.parent()?;
    }
    let mut normalized = ancestor.canonicalize().ok()?;
    for component in suffix.into_iter().rev() {
        normalized.push(component);
    }
    Some(normalized)
}

fn path_is_within(candidate: &Path, root: &Path) -> bool {
    let candidate = candidate
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase();
    let root = root.to_string_lossy().replace('/', "\\").to_lowercase();
    candidate == root || candidate.starts_with(&format!("{}\\", root.trim_end_matches('\\')))
}

#[cfg(test)]
mod tests {
    use super::{
        path_is_within, replace_qpc_placeholder, validate_output_path, Probe, QPC_PLACEHOLDER,
    };
    use std::path::Path;

    #[test]
    fn output_path_rejects_identity_and_protected_trees() {
        let temp = std::env::temp_dir().join(format!("agentpad-perf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let identity = temp.join("identity");
        let protected = temp.join("Program Files/AgentsPads/state");
        std::fs::create_dir_all(&identity).unwrap();
        std::fs::create_dir_all(&protected).unwrap();
        assert!(validate_output_path(
            &identity.join("trace.json"),
            &identity,
            Some(&protected),
            None,
        )
        .is_none());
        assert!(validate_output_path(
            &protected.join("trace.json"),
            &identity,
            Some(&protected),
            None,
        )
        .is_none());
        assert!(
            validate_output_path(&temp.join("trace.json"), &identity, Some(&protected), None,)
                .is_some()
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn path_guard_uses_component_boundary() {
        assert!(path_is_within(
            Path::new("C:\\A\\B\\trace.json"),
            Path::new("C:\\A\\B")
        ));
        assert!(!path_is_within(
            Path::new("C:\\A\\BC\\trace.json"),
            Path::new("C:\\A\\B")
        ));
    }

    #[test]
    fn event_buffer_keeps_final_exit_events_and_reports_overflow() {
        let probe = Probe::for_test(1, 1);
        probe.record("ordinary_one", None, None, None, None, None);
        probe.record("ordinary_two", None, None, None, None, None);
        probe.record("before_process_exit", None, None, None, None, None);
        let names = probe.event_names_for_test();
        assert!(names.contains(&"before_process_exit"));
        assert!(!names.contains(&"ordinary_two"));
        assert_eq!(probe.dropped_events_for_test(), 1);
    }

    #[test]
    fn qpc_footer_patch_keeps_json_valid_and_fixed_width() {
        let source = format!(
            "{{\"serialization\":{},\"write\":{}}}",
            std::str::from_utf8(QPC_PLACEHOLDER).unwrap(),
            std::str::from_utf8(QPC_PLACEHOLDER).unwrap()
        );
        let mut bytes = source.into_bytes();
        let original_len = bytes.len();
        replace_qpc_placeholder(&mut bytes, 1234).unwrap();
        replace_qpc_placeholder(&mut bytes, 5678).unwrap();
        assert_eq!(bytes.len(), original_len);
        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(document["serialization"].as_i64(), Some(1234));
        assert_eq!(document["write"].as_i64(), Some(5678));
    }

    #[test]
    fn final_snapshot_contains_serialization_and_bulk_write_qpc_bounds() {
        let path =
            std::env::temp_dir().join(format!("agentpad-perf-trace-{}.json", uuid::Uuid::new_v4()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let probe = Probe::for_test(16, 8);
        probe.output.lock().unwrap().replace(file);
        probe.record("before_process_exit", None, None, None, None, None);
        probe.finalize().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let started = document["snapshot_started_qpc"].as_i64().unwrap();
        let serialized = document["snapshot_serialization_finished_qpc"]
            .as_i64()
            .unwrap();
        let written = document["snapshot_bulk_write_flush_finished_qpc"]
            .as_i64()
            .unwrap();
        assert!(started <= serialized);
        assert!(serialized <= written);
        assert_eq!(document["snapshot_footer_patch_write_excluded"], true);
        assert_eq!(document["queue_scope"], "desktop_pointer_injection_mpsc");
        assert!(document["host_process_created_filetime_100ns"].is_u64());
        assert!(document["event_counts"]["trace_snapshot_begin"]
            .as_u64()
            .is_some());

        drop(probe);
        std::fs::remove_file(path).unwrap();
    }
}
