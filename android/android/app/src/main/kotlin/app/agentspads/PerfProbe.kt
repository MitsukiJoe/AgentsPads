package app.agentspads

import android.content.Context
import android.os.SystemClock
import android.util.AtomicFile
import okhttp3.WebSocket
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

internal data class PointerQueueSnapshot(
    val segmentCount: Int,
    val oldestAgeNs: Long,
)

internal data class ConnectionQueueSnapshot(
    val connectionSeq: Long,
    val queueBytes: Long,
)

internal data class PerfProbeState(
    val sampledAtNs: Long,
    val snapshotWindowStartNs: Long,
    val snapshotWindowEndNs: Long,
    val pointer: PointerQueueSnapshot,
    val connections: List<ConnectionQueueSnapshot>,
) {
    val okhttpQueueBytes: Long
        get() = connections.sumOf { it.queueBytes.coerceAtLeast(0) }
}

internal interface PointerPumpProbe {
    val enabled: Boolean

    fun onEnqueue(id: String, atNs: Long, segmentDepth: Int, oldestAgeNs: Long, coalesced: Boolean)

    fun onWebSocketSend(
        id: String,
        enqueuedAtNs: Long,
        takeAtNs: Long,
        queueSegmentsAfterTake: Int,
        oldestAgeAfterTakeNs: Long,
        pumpScheduledAtNs: Long?,
        runStartedAtNs: Long?,
        sendStartedAtNs: Long,
        sendEndedAtNs: Long,
        queueBeforeBytes: Long,
        queueAfterBytes: Long,
        accepted: Boolean,
    )

    fun onSendSkipped(id: String, enqueuedAtNs: Long, takeAtNs: Long)
    fun onSegmentsDropped(count: Int, reason: String)
}

internal class BoundedRingBuffer<T>(private val capacity: Int) {
    private val values = ArrayDeque<T>()
    var droppedCount: Long = 0
        private set

    init {
        require(capacity > 0)
    }

    fun add(value: T) {
        if (values.size == capacity) {
            values.removeFirst()
            droppedCount++
        }
        values.addLast(value)
    }

    fun snapshot(): List<T> = values.toList()

    fun clear() {
        values.clear()
        droppedCount = 0
    }

    val size: Int
        get() = values.size
}

internal object PerfProbeGate {
    fun shouldEnable(debugBuild: Boolean, markerExists: Boolean): Boolean =
        debugBuild && markerExists
}

internal object PerfProbeAuthGate {
    fun isAuthenticatedConnectedMessage(
        debugBuild: Boolean,
        sourceSocketMatches: Boolean,
        text: String,
    ): Boolean {
        if (!debugBuild || !sourceSocketMatches) return false
        return runCatching { JSONObject(text).optString("type") == "connected" }.getOrDefault(false)
    }
}

internal object PerfProbe : PointerPumpProbe {
    private const val SCHEMA_VERSION = 1
    private const val BUFFER_CAPACITY = 65_536
    private const val MARKER_NAME = "perf-probe.enable"
    private const val TRACE_NAME = "perf-probe.json"

    private data class Source(
        val pump: PointerPump,
        val sockets: ConcurrentHashMap<String, WebSocket>,
    )

    private val lock = Any()
    private val bufferedEvents = BoundedRingBuffer<JSONObject>(BUFFER_CAPACITY)
    private val connectionSequences = HashMap<String, Long>()
    private val readyConnections = ConcurrentHashMap<String, WebSocket>()
    private val authenticatedConnections = ConcurrentHashMap<String, WebSocket>()
    @Volatile private var active = false
    @Volatile private var source: Source? = null
    private var runId = ""
    private var runStartedAtNs = 0L
    private var gestureEndedAtNs = 0L
    private var nextConnectionSequence = 0L
    private var nextEventSequence = 0L
    private var pointerAddCalls = 0L
    private var segmentCreates = 0L
    private var coalescedAddCalls = 0L
    private var websocketSendCalls = 0L
    private var websocketSendAccepted = 0L
    private var websocketSendRejected = 0L
    private var sendSkippedNoSocket = 0L
    private var droppedSegments = 0L
    private var maxPointerSegmentDepth = 0
    private var maxPointerOldestAgeNs = 0L
    private var maxOkHttpQueueBytes = 0L
    private var okhttpQueueSizeUnavailableSamples = 0L

    override val enabled: Boolean
        get() = active

    fun attach(pump: PointerPump, sockets: ConcurrentHashMap<String, WebSocket>) {
        source = Source(pump, sockets)
        if (!active) return
        synchronized(lock) {
            if (!active) return
            sockets.forEach { (id, socket) -> ensureConnectionLocked(id, socket) }
        }
    }

    fun refreshEnabled(ctx: Context): Boolean {
        val enabled = PerfProbeGate.shouldEnable(
            isDebuggable(ctx),
            File(ctx.filesDir, MARKER_NAME).isFile,
        )
        if (!enabled) {
            active = false
            return false
        }
        if (active) return true
        return beginMarkedRun(ctx)
    }

    fun beginMarkedRun(ctx: Context): Boolean {
        if (!PerfProbeGate.shouldEnable(
                isDebuggable(ctx),
                File(ctx.filesDir, MARKER_NAME).isFile,
            )
        ) {
            active = false
            return false
        }

        val nowNs = SystemClock.elapsedRealtimeNanos()
        synchronized(lock) {
            resetLocked()
            runId = UUID.randomUUID().toString()
            runStartedAtNs = nowNs
            active = true
            appendLocked(event("run_start", nowNs))
            source?.sockets?.forEach { (id, socket) -> ensureConnectionLocked(id, socket) }
        }
        return true
    }

    fun connectionOpened(id: String, socket: WebSocket) {
        authenticatedConnections.remove(id)
        readyConnections[id] = socket
        registerConnection(id, socket)
        if (!active) return
        synchronized(lock) {
            if (active) {
                val seq = connectionSequences[id]
                if (seq != null) {
                    appendLocked(
                        event("connection_ready", SystemClock.elapsedRealtimeNanos())
                            .put("connection_seq", seq),
                    )
                }
            }
        }
    }

    fun hasOpenConnection(): Boolean = readyConnections.any { (id, socket) ->
        source?.sockets?.get(id) === socket
    }

    fun openConnectionCount(): Int = readyConnections.count { (id, socket) ->
        source?.sockets?.get(id) === socket
    }

    fun hasAuthenticatedConnection(): Boolean = authenticatedConnections.any { (id, socket) ->
        source?.sockets?.get(id) === socket
    }

    fun authenticatedConnectionCount(): Int = authenticatedConnections.count { (id, socket) ->
        source?.sockets?.get(id) === socket
    }

    fun observeAuthenticatedMessage(id: String, socket: WebSocket, text: String, debugBuild: Boolean) {
        val currentSource = source ?: return
        if (PerfProbeAuthGate.isAuthenticatedConnectedMessage(
                debugBuild,
                currentSource.sockets[id] === socket,
                text,
            )
        ) {
            authenticatedConnections[id] = socket
        }
    }

    fun queuesDrained(): Boolean {
        val currentSource = source ?: return false
        if (currentSource.pump.queueSnapshot(0).segmentCount != 0) return false
        return currentSource.sockets.values.all { socket ->
            runCatching { socket.queueSize() == 0L }.getOrDefault(false)
        }
    }
    override fun onEnqueue(
        id: String,
        atNs: Long,
        segmentDepth: Int,
        oldestAgeNs: Long,
        coalesced: Boolean,
    ) {
        if (!active) return
        synchronized(lock) {
            if (!active) return
            pointerAddCalls++
            if (coalesced) coalescedAddCalls++ else segmentCreates++
            maxPointerSegmentDepth = maxOf(maxPointerSegmentDepth, segmentDepth)
            maxPointerOldestAgeNs = maxOf(maxPointerOldestAgeNs, oldestAgeNs)
            val connectionSeq = connectionSequences[id]
                ?: source?.sockets?.get(id)?.let { ensureConnectionLocked(id, it) }
            val sample = event("enqueue", atNs)
                .put("coalesced", coalesced)
                .put("queue_segment_depth", segmentDepth)
                .put("oldest_age_ns", oldestAgeNs)
            putConnectionSeq(sample, connectionSeq)
            appendLocked(sample)
        }
    }

    override fun onWebSocketSend(
        id: String,
        enqueuedAtNs: Long,
        takeAtNs: Long,
        queueSegmentsAfterTake: Int,
        oldestAgeAfterTakeNs: Long,
        pumpScheduledAtNs: Long?,
        runStartedAtNs: Long?,
        sendStartedAtNs: Long,
        sendEndedAtNs: Long,
        queueBeforeBytes: Long,
        queueAfterBytes: Long,
        accepted: Boolean,
    ) {
        if (!active) return
        synchronized(lock) {
            if (!active) return
            websocketSendCalls++
            if (accepted) websocketSendAccepted++ else websocketSendRejected++
            if (queueBeforeBytes < 0 || queueAfterBytes < 0) okhttpQueueSizeUnavailableSamples++
            maxOkHttpQueueBytes = maxOf(
                maxOkHttpQueueBytes,
                queueBeforeBytes.coerceAtLeast(0),
                queueAfterBytes.coerceAtLeast(0),
            )
            maxPointerSegmentDepth = maxOf(maxPointerSegmentDepth, queueSegmentsAfterTake)
            maxPointerOldestAgeNs = maxOf(maxPointerOldestAgeNs, oldestAgeAfterTakeNs)
            val sample = event("websocket_send", sendStartedAtNs)
                .put("enqueue_ns", enqueuedAtNs)
                .put("take_ns", takeAtNs)
                .put("send_start_ns", sendStartedAtNs)
                .put("send_end_ns", sendEndedAtNs)
                .put("send_duration_ns", (sendEndedAtNs - sendStartedAtNs).coerceAtLeast(0))
                .put("queue_segments_after_take", queueSegmentsAfterTake)
                .put("oldest_age_after_take_ns", oldestAgeAfterTakeNs)
                .put("okhttp_queue_before_bytes", queueBeforeBytes)
                .put("okhttp_queue_after_bytes", queueAfterBytes)
                .put("accepted", accepted)
            val runAt = runStartedAtNs
            if (runAt != null) sample.put("run_started_ns", runAt)
            val scheduledAt = pumpScheduledAtNs
            if (scheduledAt != null && runAt != null && runAt >= scheduledAt &&
                sendStartedAtNs >= runAt
            ) {
                sample
                    .put("pump_scheduled_ns", scheduledAt)
                    .put("scheduled_to_run_ns", runAt - scheduledAt)
                    .put("run_to_send_ns", sendStartedAtNs - runAt)
                    .put("scheduled_to_send_ns", sendStartedAtNs - scheduledAt)
            }
            putConnectionSeq(sample, connectionSequences[id])
            appendLocked(sample)
        }
    }

    override fun onSendSkipped(id: String, enqueuedAtNs: Long, takeAtNs: Long) {
        if (!active) return
        synchronized(lock) {
            if (!active) return
            sendSkippedNoSocket++
            val sample = event("send_skipped_no_socket", takeAtNs)
                .put("enqueue_ns", enqueuedAtNs)
                .put("take_ns", takeAtNs)
            putConnectionSeq(sample, connectionSequences[id])
            appendLocked(sample)
        }
    }

    override fun onSegmentsDropped(count: Int, reason: String) {
        if (!active || count <= 0) return
        synchronized(lock) {
            if (!active) return
            droppedSegments += count
            appendLocked(
                event("segments_dropped", SystemClock.elapsedRealtimeNanos())
                    .put("count", count)
                    .put("reason", reason),
            )
        }
    }

    fun registerConnection(id: String, socket: WebSocket) {
        if (!active) return
        synchronized(lock) {
            if (active) ensureConnectionLocked(id, socket)
        }
    }

    fun connectionEnded(id: String, socket: WebSocket, reason: String) {
        authenticatedConnections.remove(id, socket)
        if (!active) return
        val atNs = SystemClock.elapsedRealtimeNanos()
        val queueBytes = runCatching { socket.queueSize() }.getOrDefault(-1L)
        synchronized(lock) {
            if (queueBytes < 0) okhttpQueueSizeUnavailableSamples++
            if (!active) return
            val connectionSeq = connectionSequences.remove(id)
            val sample = event("connection_end", atNs)
                .put("reason", reason)
                .put("okhttp_queue_bytes", queueBytes)
            putConnectionSeq(sample, connectionSeq)
            updateMaxQueueLocked(queueBytes)
            appendLocked(sample)
        }
    }

    fun gestureBoundary(
        sampleType: String,
        atNs: Long,
        plannedDurationMs: Long,
        stepMs: Long,
        remoteAmplitudePerLeg: Double,
        openConnectionCount: Int = 0,
        injectedMoves: Long = 0,
        measuredGestureNs: Long = 0,
        upDelivered: Boolean = true,
    ) {
        if (!active || sampleType !in setOf("gesture_start", "gesture_end")) return
        synchronized(lock) {
            if (!active) return
            val sample = event(sampleType, atNs)
                .put("planned_duration_ms", plannedDurationMs)
                .put("step_ms", stepMs)
                .put("remote_amplitude_per_leg", remoteAmplitudePerLeg)
                .put("open_connection_count", openConnectionCount)
            if (sampleType == "gesture_end") {
                gestureEndedAtNs = atNs
                sample
                    .put("injected_move_events", injectedMoves)
                    .put("measured_gesture_ns", measuredGestureNs)
                    .put("up_delivered", upDelivered)
            }
            appendLocked(sample)
        }
    }

    fun readCurrentState(): PerfProbeState? {
        if (!active) return null
        val currentSource = source
        val startNs = SystemClock.elapsedRealtimeNanos()
        val pointer = currentSource?.pump?.queueSnapshot(startNs)
            ?: PointerQueueSnapshot(0, 0)
        val connectionSamples = currentSource?.sockets?.entries
            ?.mapNotNull { (id, socket) ->
                val seq = synchronized(lock) {
                    if (!active) return@synchronized null
                    connectionSequences[id]
                        ?: ensureConnectionLocked(id, socket)
                } ?: return@mapNotNull null
                val queueBytes = runCatching { socket.queueSize() }.getOrDefault(-1L)
                if (queueBytes >= 0) synchronized(lock) { updateMaxQueueLocked(queueBytes) }
                ConnectionQueueSnapshot(seq, queueBytes)
            }
            .orEmpty()
            .sortedBy { it.connectionSeq }
        val endNs = SystemClock.elapsedRealtimeNanos()
        if (!active) return null
        return PerfProbeState(
            sampledAtNs = endNs,
            snapshotWindowStartNs = startNs,
            snapshotWindowEndNs = endNs,
            pointer = pointer,
            connections = connectionSamples,
        )
    }

    fun recordIdleSnapshot(): PerfProbeState? {
        val state = readCurrentState() ?: return null
        synchronized(lock) {
            if (!active) return null
            val sample = event("idle_snapshot", state.sampledAtNs)
                .put("snapshot_window_start_ns", state.snapshotWindowStartNs)
                .put("snapshot_window_end_ns", state.snapshotWindowEndNs)
                .put("pointer_segments", state.pointer.segmentCount)
                .put("pointer_oldest_age_ns", state.pointer.oldestAgeNs)
                .put("okhttp_queue_bytes", state.okhttpQueueBytes)
            if (gestureEndedAtNs > 0) {
                sample.put("idle_after_gesture_ns", (state.sampledAtNs - gestureEndedAtNs).coerceAtLeast(0))
            }
            val connections = JSONArray()
            state.connections.forEach {
                connections.put(
                    JSONObject()
                        .put("connection_seq", it.connectionSeq)
                        .put("okhttp_queue_bytes", it.queueBytes),
                )
            }
            sample.put("connections", connections)
            appendLocked(sample)
        }
        return state
    }

    fun endMarkedRun() {
        active = false
        synchronized(lock) {
            resetLocked()
            runId = ""
            runStartedAtNs = 0
        }
    }

    fun snapshotAndWrite(ctx: Context): JSONObject? {
        if (!isDebuggable(ctx) ||
            !File(ctx.filesDir, MARKER_NAME).isFile ||
            !active
        ) {
            return null
        }
        val state = recordIdleSnapshot() ?: return null
        val anchorBeforeNs = SystemClock.elapsedRealtimeNanos()
        val utcEpochMs = System.currentTimeMillis()
        val anchorAfterNs = SystemClock.elapsedRealtimeNanos()
        val anchorElapsedNs = anchorBeforeNs + (anchorAfterNs - anchorBeforeNs) / 2
        val root = synchronized(lock) {
            if (!active || !File(ctx.filesDir, MARKER_NAME).isFile) return null
            val pointer = JSONObject()
                .put("add_calls", pointerAddCalls)
                .put("segments_created", segmentCreates)
                .put("coalesced_add_calls", coalescedAddCalls)
                .put("send_calls", websocketSendCalls)
                .put("send_accepted", websocketSendAccepted)
                .put("send_rejected", websocketSendRejected)
                .put("send_skipped_no_socket", sendSkippedNoSocket)
                .put("segments_dropped", droppedSegments)
                .put("current_segments", state.pointer.segmentCount)
                .put("current_oldest_age_ns", state.pointer.oldestAgeNs)
                .put("max_segment_depth", maxPointerSegmentDepth)
                .put("max_oldest_age_ns", maxPointerOldestAgeNs)
            val okhttp = JSONObject()
                .put("current_queued_bytes", state.okhttpQueueBytes)
                .put("current_unavailable_queue_count", state.connections.count { it.queueBytes < 0 })
                .put("queue_size_unavailable_samples", okhttpQueueSizeUnavailableSamples)
                .put("max_observed_queued_bytes", maxOkHttpQueueBytes)
            val summary = JSONObject()
                .put("run_started_ns", runStartedAtNs)
                .put("snapshot_ns", state.sampledAtNs)
                .put("pointer_pump", pointer)
                .put("okhttp_application_queue", okhttp)
                .put("max_buffered_events", BUFFER_CAPACITY)
                .put("buffered_events", bufferedEvents.size)
                .put("overflow_dropped_events", bufferedEvents.droppedCount)
            val eventSnapshot = bufferedEvents.snapshot()
            val monotonicWindowStartNs = eventSnapshot.minOfOrNull { it.optLong("t_ns", state.sampledAtNs) }
                ?: state.sampledAtNs
            val pumpRunnableStarts = eventSnapshot.asSequence()
                .filter { it.optString("sample_type") == "websocket_send" }
                .mapNotNull { if (it.has("run_started_ns")) it.optLong("run_started_ns") else null }
                .toSet()
            summary
                .put("monotonic_window_start_ns", monotonicWindowStartNs)
                .put("monotonic_window_end_ns", state.sampledAtNs)
                .put("monotonic_window_duration_ns", (state.sampledAtNs - monotonicWindowStartNs).coerceAtLeast(0))
                .put("pump_runnable_runs_observed", pumpRunnableStarts.size)
                .put("frame_callback_samples", 0)
                .put("frame_callback_timing", "not_available_no_frame_callback_in_pointerpump")
                .put("transport_scope", "WebSocket.send call/return and OkHttp application queueSize bytes; not TCP or wire timing")
                .put("event_order_basis", "append_seq is lock recording order; t_ns preserves measured monotonic time; recorded_at_ns is append time")
                .put("send_timing_boundary", "send_start_ns immediately before WebSocket.send; send_end_ns immediately after it returns or throws")
            val eventArray = JSONArray()
            eventSnapshot.forEach(eventArray::put)
            JSONObject()
                .put("schema_version", SCHEMA_VERSION)
                .put("run_id", runId)
                .put("clock", "android_elapsed_realtime_nanos")
                .put(
                    "wall_clock_anchor",
                    JSONObject()
                        .put("utc_epoch_ms", utcEpochMs)
                        .put("elapsed_realtime_ns", anchorElapsedNs)
                        .put("pair_sampling_span_ns", anchorAfterNs - anchorBeforeNs),
                )
                .put("summary", summary)
                .put("events", eventArray)
        }

        val atomicFile = AtomicFile(File(ctx.filesDir, TRACE_NAME))
        var stream: java.io.FileOutputStream? = null
        try {
            stream = atomicFile.startWrite()
            stream.write(root.toString().toByteArray(Charsets.UTF_8))
            atomicFile.finishWrite(stream)
            stream = null
        } catch (error: Throwable) {
            stream?.let(atomicFile::failWrite)
            throw error
        }
        return root
    }

    private fun isDebuggable(ctx: Context): Boolean =
        (ctx.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0

    private fun ensureConnectionLocked(id: String, socket: WebSocket): Long {
        connectionSequences[id]?.let { return it }
        val seq = ++nextConnectionSequence
        connectionSequences[id] = seq
        val queueBytes = runCatching { socket.queueSize() }.getOrDefault(-1L)
        updateMaxQueueLocked(queueBytes)
        appendLocked(
            event("connection_registered", SystemClock.elapsedRealtimeNanos())
                .put("connection_seq", seq)
                .put("okhttp_queue_bytes", queueBytes),
        )
        return seq
    }

    private fun event(type: String, atNs: Long): JSONObject =
        JSONObject()
            .put("sample_type", type)
            .put("t_ns", atNs)

    private fun putConnectionSeq(event: JSONObject, connectionSeq: Long?) {
        if (connectionSeq != null) event.put("connection_seq", connectionSeq)
    }

    private fun appendLocked(event: JSONObject) {
        event.put("append_seq", ++nextEventSequence)
            .put("recorded_at_ns", SystemClock.elapsedRealtimeNanos())
        bufferedEvents.add(event)
    }

    private fun updateMaxQueueLocked(queueBytes: Long) {
        if (queueBytes >= 0) maxOkHttpQueueBytes = maxOf(maxOkHttpQueueBytes, queueBytes)
    }

    private fun resetLocked() {
        bufferedEvents.clear()
        connectionSequences.clear()
        nextConnectionSequence = 0
        nextEventSequence = 0
        gestureEndedAtNs = 0
        pointerAddCalls = 0
        segmentCreates = 0
        coalescedAddCalls = 0
        websocketSendCalls = 0
        websocketSendAccepted = 0
        websocketSendRejected = 0
        sendSkippedNoSocket = 0
        droppedSegments = 0
        maxPointerSegmentDepth = 0
        maxPointerOldestAgeNs = 0
        maxOkHttpQueueBytes = 0
        okhttpQueueSizeUnavailableSamples = 0
    }
}
