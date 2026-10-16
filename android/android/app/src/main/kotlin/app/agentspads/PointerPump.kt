package app.agentspads

import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import okhttp3.WebSocket
import org.json.JSONObject
import java.util.ArrayDeque
import java.util.concurrent.ConcurrentHashMap

internal class PointerPump(
    private val sockets: ConcurrentHashMap<String, WebSocket>,
    scheduler: ((Runnable) -> Unit)? = null,
    private val sendPacket: ((String, String) -> Boolean)? = null,
    private val probe: PointerPumpProbe? = null,
) : Runnable {
    private data class Segment(
        var dx: Double,
        var dy: Double,
        val buttons: Int,
        var wheel: Int,
        val buttonEdge: Boolean,
        val enqueuedAtNs: Long,
    )

    private data class Packet(
        val id: String,
        val json: String,
        val socket: WebSocket?,
        val enqueuedAtNs: Long,
        val takeAtNs: Long,
        val pumpScheduledAtNs: Long?,
        val queueSegmentsAfterTake: Int,
        val oldestAgeAfterTakeNs: Long,
    )

    private val lock = Any()
    private val queues = HashMap<String, ArrayDeque<Segment>>()
    private val buttonState = HashMap<String, Int>()
    private val pending = LinkedHashSet<String>()
    private val thread: HandlerThread?
    private val handler: Handler?
    private val wake: (Runnable) -> Unit
    private var draining = false
    private var closed = false
    private var pumpScheduledAtNs: Long? = null

    init {
        if (scheduler == null) {
            thread = HandlerThread("agentpad-pointer").apply { start() }
            handler = Handler(thread.looper)
            wake = { runnable -> handler.post(runnable) }
        } else {
            thread = null
            handler = null
            wake = scheduler
        }
    }

    fun close() {
        var discarded = 0
        val activeProbe = probe?.takeIf { it.enabled }
        synchronized(lock) {
            if (closed) return
            closed = true
            if (activeProbe != null) discarded = queues.values.sumOf { it.size }
            queues.clear()
            buttonState.clear()
            pending.clear()
            draining = false
            pumpScheduledAtNs = null
        }
        if (discarded > 0) activeProbe?.onSegmentsDropped(discarded, "pump_close")
        handler?.removeCallbacksAndMessages(null)
        thread?.quitSafely()
    }

    fun add(id: String, ddx: Double, ddy: Double, btn: Int, wh: Int, immediate: Boolean) {
        val activeProbe = probe?.takeIf { it.enabled }
        val atNs = if (activeProbe != null) SystemClock.elapsedRealtimeNanos() else 0L
        var schedule = false
        var coalesced = false
        var segmentDepth = 0
        var oldestAgeNs = 0L
        synchronized(lock) {
            if (closed) return
            val edge = (buttonState.put(id, btn) ?: 0) != btn
            val queue = queues.getOrPut(id) { ArrayDeque() }
            val last = queue.peekLast()
            if (last != null && !last.buttonEdge && !edge && last.buttons == btn) {
                last.dx += ddx
                last.dy += ddy
                last.wheel += wh
                coalesced = true
            } else {
                queue.addLast(Segment(ddx, ddy, btn, wh, edge, atNs))
            }
            pending.add(id)
            if (activeProbe != null) {
                segmentDepth = queue.size
                oldestAgeNs = (atNs - (queue.peekFirst()?.enqueuedAtNs ?: atNs)).coerceAtLeast(0)
            }
            if (!draining) {
                draining = true
                pumpScheduledAtNs = atNs.takeIf { activeProbe != null }
                schedule = true
            }
        }
        activeProbe?.onEnqueue(id, atNs, segmentDepth, oldestAgeNs, coalesced)
        if (schedule) wake(this)
    }

    fun drop(id: String) {
        val discarded = synchronized(lock) {
            pending.remove(id)
            val removed = queues.remove(id)
            val count = if (probe?.enabled == true) removed?.size ?: 0 else 0
            buttonState.remove(id)
            count
        }
        if (discarded > 0 && probe?.enabled == true) probe.onSegmentsDropped(discarded, "connection_drop")
    }

    internal fun queueSnapshot(sampleAtNs: Long = SystemClock.elapsedRealtimeNanos()): PointerQueueSnapshot =
        synchronized(lock) {
            var segments = 0
            var oldestEnqueuedAtNs = Long.MAX_VALUE
            for (queue in queues.values) {
                segments += queue.size
                val oldest = queue.peekFirst()?.enqueuedAtNs ?: continue
                oldestEnqueuedAtNs = minOf(oldestEnqueuedAtNs, oldest)
            }
            PointerQueueSnapshot(
                segmentCount = segments,
                oldestAgeNs = if (oldestEnqueuedAtNs == Long.MAX_VALUE) {
                    0
                } else {
                    (sampleAtNs - oldestEnqueuedAtNs).coerceAtLeast(0)
                },
            )
        }

    override fun run() {
        val runStartedAtNs = if (probe?.enabled == true) SystemClock.elapsedRealtimeNanos() else null
        while (true) {
            val batch = synchronized(lock) {
                if (closed || pending.isEmpty()) {
                    draining = false
                    pumpScheduledAtNs = null
                    return
                }
                val ids = pending.toList()
                pending.clear()
                ids.mapNotNull { id -> takeLocked(id) }
            }
            for (packet in batch) {
                val callback = sendPacket
                if (callback != null) {
                    callback.invoke(packet.id, packet.json)
                    continue
                }
                val socket = packet.socket
                if (socket == null) {
                    if (probe?.enabled == true) probe.onSendSkipped(packet.id, packet.enqueuedAtNs, packet.takeAtNs)
                    continue
                }
                val activeProbe = probe?.takeIf { it.enabled }
                val queueBeforeBytes = if (activeProbe != null) runCatching { socket.queueSize() }.getOrDefault(-1L) else -1L
                val sendStartedAtNs = if (activeProbe != null) SystemClock.elapsedRealtimeNanos() else 0L
                var accepted = false
                var failure: Throwable? = null
                try {
                    accepted = socket.send(packet.json)
                } catch (error: Throwable) {
                    failure = error
                }
                val sendEndedAtNs = if (activeProbe != null) SystemClock.elapsedRealtimeNanos() else 0L
                val queueAfterBytes = if (activeProbe != null) runCatching { socket.queueSize() }.getOrDefault(-1L) else -1L
                activeProbe?.onWebSocketSend(
                    id = packet.id,
                    enqueuedAtNs = packet.enqueuedAtNs,
                    takeAtNs = packet.takeAtNs,
                    queueSegmentsAfterTake = packet.queueSegmentsAfterTake,
                    oldestAgeAfterTakeNs = packet.oldestAgeAfterTakeNs,
                    pumpScheduledAtNs = packet.pumpScheduledAtNs,
                    runStartedAtNs = runStartedAtNs,
                    sendStartedAtNs = sendStartedAtNs,
                    sendEndedAtNs = sendEndedAtNs,
                    queueBeforeBytes = queueBeforeBytes,
                    queueAfterBytes = queueAfterBytes,
                    accepted = accepted,
                )
                failure?.let { throw it }
            }
        }
    }

    private fun takeLocked(id: String): Packet? {
        val queue = queues[id] ?: return null
        val segment = queue.removeFirst()
        if (queue.isEmpty()) queues.remove(id) else pending.add(id)
        val measureTiming = probe?.enabled == true
        val takeAtNs = if (measureTiming) SystemClock.elapsedRealtimeNanos() else 0L
        val oldestAgeAfterTakeNs = if (measureTiming) queue.peekFirst()?.let {
            (takeAtNs - it.enqueuedAtNs).coerceAtLeast(0)
        } ?: 0 else 0
        return Packet(
            id = id,
            json = JSONObject()
                .put("type", "pointer")
                .put("dx", segment.dx)
                .put("dy", segment.dy)
                .put("buttons", segment.buttons)
                .put("wheel", segment.wheel)
                .toString(),
            socket = sockets[id],
            enqueuedAtNs = segment.enqueuedAtNs,
            takeAtNs = takeAtNs,
            pumpScheduledAtNs = pumpScheduledAtNs,
            queueSegmentsAfterTake = if (measureTiming) queue.size else 0,
            oldestAgeAfterTakeNs = oldestAgeAfterTakeNs,
        )
    }
}
