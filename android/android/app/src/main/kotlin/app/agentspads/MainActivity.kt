package app.agentspads

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.media.AudioManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.renderer.FlutterUiDisplayListener
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.net.Inet4Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketAddress
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.net.SocketFactory

class MainActivity : FlutterActivity() {
    private lateinit var voiceRecording: VoiceRecordingTracker
    private var ws: WifiWs? = null
    private var inputView: BackspaceFlutterView? = null
    private var firstFrameRendered = false

    override fun attachToEngineAutomatically() = false

    override fun onCreate(savedInstanceState: android.os.Bundle?) {
        super.onCreate(savedInstanceState)
        runCatching { LauncherIcon.onColdStart(this) }
        inputView = BackspaceFlutterView(this) { ws?.emitInputBackspace() }.also { view ->
            view.id = FLUTTER_VIEW_ID
            view.addOnFirstFrameRenderedListener(object : FlutterUiDisplayListener {
                override fun onFlutterUiDisplayed() {
                    firstFrameRendered = true
                    this@MainActivity.onFlutterUiDisplayed()
                }

                override fun onFlutterUiNoLongerDisplayed() {
                    this@MainActivity.onFlutterUiNoLongerDisplayed()
                }
            })
            setContentView(view)
            view.attachToFlutterEngine(requireNotNull(flutterEngine))
        }
        preferPeakRefreshRate()
        window.decorView.post { preferPeakRefreshRate() }
    }

    override fun onStart() {
        super.onStart()
        ws?.setVisible(true)
        inputView?.visibility = View.VISIBLE
    }

    override fun onStop() {
        ws?.setVisible(false)
        inputView?.visibility = View.GONE
        super.onStop()
    }

    override fun onDestroy() {
        inputView?.detachFromFlutterEngine()
        inputView = null
        super.onDestroy()
    }

    override fun detachFromFlutterEngine() {
        inputView?.detachFromFlutterEngine()
        super.detachFromFlutterEngine()
    }

    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        if (inputView?.isAttachedToFlutterEngine == true &&
            firstFrameRendered && level >= TRIM_MEMORY_RUNNING_LOW
        ) {
            flutterEngine?.dartExecutor?.notifyLowMemoryWarning()
            flutterEngine?.systemChannel?.sendMemoryPressureWarning()
        }
    }

    override fun onResume() {
        super.onResume()
        preferPeakRefreshRate()
        window.decorView.post { preferPeakRefreshRate() }
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) preferPeakRefreshRate()
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        preferPeakRefreshRate()
        window.decorView.post { preferPeakRefreshRate() }
        voiceRecording = VoiceRecordingTracker(this).also { it.start() }
        ws = WifiWs(this, flutterEngine, voiceRecording)
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        ws?.dispose()
        ws = null
        if (::voiceRecording.isInitialized) voiceRecording.stop()
        super.cleanUpFlutterEngine(flutterEngine)
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        window.decorView.requestUnbufferedDispatch(event)
        return super.dispatchTouchEvent(event)
    }

    // Flutter can stay at 60Hz unless both the window and its render surface ask for the peak mode.
    private fun preferPeakRefreshRate() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return
        val display =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                display
            } else {
                @Suppress("DEPRECATION")
                windowManager.defaultDisplay
            } ?: return
        val current = display.mode
        val best =
            display.supportedModes
                .filter {
                    it.physicalWidth == current.physicalWidth &&
                        it.physicalHeight == current.physicalHeight
                }
                .maxByOrNull { it.refreshRate }
                ?: display.supportedModes.maxByOrNull { it.refreshRate }
                ?: return
        val attrs = window.attributes
        attrs.preferredDisplayModeId = best.modeId
        @Suppress("DEPRECATION")
        attrs.preferredRefreshRate = best.refreshRate
        window.attributes = attrs
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            requestFrameRate(window.decorView, best.refreshRate)
        }
    }

    @android.annotation.TargetApi(Build.VERSION_CODES.R)
    private fun requestFrameRate(view: android.view.View, hz: Float) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM) {
            runCatching { view.requestedFrameRate = hz }
        }
        if (view is android.view.SurfaceView) {
            val surface = view.holder.surface
            if (surface.isValid) {
                runCatching {
                    surface.setFrameRate(hz, android.view.Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE)
                }
            }
        }
        if (view is android.view.ViewGroup) {
            for (i in 0 until view.childCount) requestFrameRate(view.getChildAt(i), hz)
        }
    }
}

class LauncherTrampolineActivity : android.app.Activity() {
    override fun onCreate(savedInstanceState: android.os.Bundle?) {
        super.onCreate(savedInstanceState)

        val mainTask = getSystemService(android.app.ActivityManager::class.java)
            .appTasks
            .firstOrNull { task ->
                task.taskInfo.id != taskId &&
                    (task.taskInfo.baseActivity?.className == MainActivity::class.java.name ||
                        task.taskInfo.topActivity?.className == MainActivity::class.java.name)
            }
        val returnedToExistingTask = mainTask?.let {
            runCatching { it.moveToFront() }.isSuccess
        } == true

        if (!returnedToExistingTask) {
            runCatching {
                startActivity(
                    Intent(this, MainActivity::class.java).addFlags(
                        Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK,
                    ),
                )
                overridePendingTransition(0, 0)
            }
        }

        finishAndRemoveTask()
        overridePendingTransition(0, 0)
    }
}

class VoiceRecordingTracker(ctx: Context) {
    private val tracker = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
        VoiceRecordingTracker24(ctx)
    } else {
        null
    }

    fun start() = tracker?.start() ?: Unit

    fun stop() = tracker?.stop() ?: Unit

    fun reset() = tracker?.reset() ?: Unit

    fun hasRecentEvidence() = tracker?.hasRecentEvidence() == true
}

@android.annotation.TargetApi(Build.VERSION_CODES.N)
private class VoiceRecordingTracker24(ctx: Context) {
    private val audio = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private val main = Handler(Looper.getMainLooper())
    private var active = false
    private var observedAfterReset = false
    private var stoppedAt = Long.MIN_VALUE
    private val callback = object : AudioManager.AudioRecordingCallback() {
        override fun onRecordingConfigChanged(configs: List<android.media.AudioRecordingConfiguration>) {
            val next = configs.isNotEmpty()
            if (!active && next) observedAfterReset = true
            if (active && !next && observedAfterReset) stoppedAt = SystemClock.elapsedRealtime()
            active = next
        }
    }

    fun start() {
        active = audio.activeRecordingConfigurations.isNotEmpty()
        audio.registerAudioRecordingCallback(callback, main)
    }

    fun stop() = audio.unregisterAudioRecordingCallback(callback)

    fun reset() {
        observedAfterReset = false
        stoppedAt = Long.MIN_VALUE
    }

    fun hasRecentEvidence(): Boolean {
        val recent = observedAfterReset &&
            (active || SystemClock.elapsedRealtime() - stoppedAt <= RECENT_RECORDING_MS)
        return recent
    }

    private companion object {
        const val RECENT_RECORDING_MS = 3000L
    }
}

private class RoutedSocket(
    private val wifi: Network? = null,
    private val reaches: ((InetAddress) -> Boolean)? = null,
) : Socket() {
    override fun connect(endpoint: SocketAddress?) {
        maybeBind(endpoint)
        super.connect(endpoint)
        lowLatency()
    }

    override fun connect(endpoint: SocketAddress?, timeout: Int) {
        maybeBind(endpoint)
        super.connect(endpoint, timeout)
        lowLatency()
    }

    private fun maybeBind(endpoint: SocketAddress?) {
        val net = wifi ?: return
        val check = reaches ?: return
        val inet = endpoint as? InetSocketAddress ?: return
        val host = inet.address ?: return
        if (check(host)) runCatching { net.bindSocket(this) }
    }

    fun lowLatency() {
        tcpNoDelay = true
        keepAlive = true
        trafficClass = 0x10
    }
}

class WifiWs(
    private val ctx: Context,
    engine: FlutterEngine,
    private val voiceRecording: VoiceRecordingTracker,
) : EventChannel.StreamHandler {
    private var visible = false
    private val debugAuthObserverEnabled =
        (ctx.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0
    private val methodChannel = MethodChannel(engine.dartExecutor.binaryMessenger, "agentpad/ws")
    private val eventChannel = EventChannel(engine.dartExecutor.binaryMessenger, "agentpad/ws_events")
    private val sockets = ConcurrentHashMap<String, WebSocket>()
    private val pointer = PointerPump(sockets, probe = PerfProbe)
    private var events: EventChannel.EventSink? = null
    private val main = Handler(Looper.getMainLooper())
    private val wifiLock: WifiManager.WifiLock? = runCatching { newWifiLock() }.getOrNull()
    private val cpuLock: PowerManager.WakeLock? =
        runCatching {
            (ctx.getSystemService(Context.POWER_SERVICE) as PowerManager)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "agentpad:cpu")
                .apply { setReferenceCounted(false) }
        }.getOrNull()

    init {
        PerfProbe.attach(pointer, sockets)
        PerfProbe.refreshEnabled(ctx)
        methodChannel.setMethodCallHandler { call, result ->
                when (call.method) {
                    "diagnosticEnabled" -> result.success(DiagnosticLog.enabled)
                    "diagnosticSetEnabled" -> result.success(DiagnosticLog.setEnabled(ctx.filesDir, call.arguments == true))
                    "diagnosticRead" -> result.success(DiagnosticLog.read(ctx.filesDir))
                    "diagnosticClear" -> {
                        result.success(runCatching { DiagnosticLog.clear(ctx.filesDir) }.isSuccess)
                    }
                    "diagnosticEvent" -> {
                        DiagnosticLog.event(call.argument<String>("kind"), call.argument<String>("stage"), call.argument<String>("result"), call.argument<Int>("count") ?: 1)
                        result.success(null)
                    }
                    "wsVisible" -> result.success(visible)
                    "connect" -> {
                        val id = call.argument<String>("id") ?: ""
                        val host = call.argument<String>("host") ?: ""
                        val port = call.argument<Int>("port") ?: 9618
                        connect(id, host, port, result)
                    }
                    "pointer" -> {
                        val id = call.argument<String>("id") ?: ""
                        DiagnosticLog.event("pointer", "send", if (visible && sockets.containsKey(id)) "ok" else "failed")
                        if (visible && sockets.containsKey(id)) pointer.add(
                            id,
                            num(call, "dx"),
                            num(call, "dy"),
                            intNum(call, "buttons"),
                            intNum(call, "wheel"),
                            call.argument<Boolean>("immediate") == true,
                        )
                        result.success(true)
                    }
                    "cameraZoomRange" -> result.success(runCatching { cameraZoomRange() }.getOrDefault(listOf(1.0, 1.0)))
                    "displayRefreshHz" -> {
                        val hz = displayRefreshHz()
                        result.success(hz)
                    }
                    "send" -> {
                        val id = call.argument<String>("id") ?: ""
                        val text = call.argument<String>("text") ?: ""
                        val ok = sockets[id]?.send(text) == true
                        if (DiagnosticLog.enabled) {
                            val kind = runCatching { org.json.JSONObject(text).optString("type") }.getOrDefault("unknown")
                            val safeKind = if (kind in setOf("text", "key", "pointer", "undo", "ping", "hello")) kind else "unknown"
                            DiagnosticLog.event(safeKind, "send", if (ok) "ok" else "failed")
                        }
                        result.success(ok)
                    }
                    "close" -> {
                        val id = call.argument<String>("id") ?: ""
                        pointer.drop(id)
                        sockets.remove(id)?.let { oldSocket ->
                            PerfProbe.connectionEnded(id, oldSocket, "app_close")
                            oldSocket.cancel()
                        }
                        releaseWifiIfIdle()
                        result.success(true)
                    }
                    "setAppIcon" -> {
                        val icon = call.argument<String>("icon") ?: "white"
                        try {
                            result.success(LauncherIcon.setPending(ctx, icon))
                        } catch (e: Exception) {
                            result.error("ICON_CHANGE_FAILED", e.message, null)
                        }
                    }
                    "getAppIconState" -> {
                        try {
                            result.success(LauncherIcon.state(ctx))
                        } catch (e: Exception) {
                            result.error("ICON_STATE_FAILED", e.message, null)
                        }
                    }
                    "finishWithAppIcon" -> {
                        try {
                            LauncherIcon.finish(ctx as android.app.Activity)
                            result.success(true)
                        } catch (e: Exception) {
                            result.error("ICON_FINISH_FAILED", e.message, null)
                        }
                    }
                    "restartWithAppIcon" -> {
                        try {
                            LauncherIcon.restart(ctx as android.app.Activity)
                            result.success(true)
                        } catch (e: Exception) {
                            result.error("ICON_RESTART_FAILED", e.message, null)
                        }
                    }
                    "openUrl" -> {
                        val url = call.argument<String>("url") ?: ""
                        try {
                            val intent = android.content.Intent(android.content.Intent.ACTION_VIEW, android.net.Uri.parse(url))
                            intent.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
                            ctx.startActivity(intent)
                            result.success(true)
                        } catch (e: Exception) {
                            result.error("OPEN_FAILED", e.message, null)
                        }
                    }
                    "voiceEvidence" -> {
                        val manager = ctx.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
                        result.success(
                            manager.currentInputMethodSubtype?.mode == "voice" ||
                                voiceRecording.hasRecentEvidence()
                        )
                    }
                    "resetVoiceEvidence" -> {
                        voiceRecording.reset()
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
        eventChannel.setStreamHandler(this)
    }

    fun setVisible(value: Boolean) {
        visible = value
        DiagnosticLog.event("lifecycle", if (value) "start" else "stop", if (value) "active" else "inactive")
        if (value) return
        for ((id, socket) in sockets) {
            if (sockets.remove(id, socket)) {
                pointer.drop(id)
                PerfProbe.connectionEnded(id, socket, "hidden")
                socket.cancel()
                emit(id, "close")
            }
        }
        releaseWifiIfIdle()
    }

    fun dispose() {
        setVisible(false)
        pointer.close()
        events = null
        methodChannel.setMethodCallHandler(null)
        eventChannel.setStreamHandler(null)
    }

    override fun onListen(arguments: Any?, events: EventChannel.EventSink?) {
        this.events = events
    }

    override fun onCancel(arguments: Any?) {
        events = null
    }

    fun emitInputBackspace() {
        main.post { events?.success(mapOf("event" to "inputBackspace")) }
    }

    private fun emit(id: String, event: String) {
        main.post { events?.success(mapOf("id" to id, "event" to event)) }
    }

    private fun connect(id: String, host: String, port: Int, result: MethodChannel.Result) {
        if (!visible) {
            result.success(false)
            return
        }
        pointer.drop(id)
        sockets.remove(id)?.let { oldSocket ->
            PerfProbe.connectionEnded(id, oldSocket, "replaced")
            oldSocket.cancel()
        }
        val settled = AtomicBoolean()
        fun settle(ok: Boolean) {
            if (settled.compareAndSet(false, true)) main.post { result.success(ok) }
        }
        val client = OkHttpClient.Builder()
            .socketFactory(pointerSocketFactory())
            .connectTimeout(4, TimeUnit.SECONDS)
            .build()
        val req = try {
            Request.Builder().url("ws://$host:$port").build()
        } catch (_: IllegalArgumentException) {
            settle(false)
            return
        }
        val ws = client.newWebSocket(
            req,
            object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    main.post {
                        val current = visible && sockets[id] === webSocket
                        if (current) {
                            PerfProbe.connectionOpened(id, webSocket)
                            holdWifi()
                        } else {
                            webSocket.cancel()
                        }
                        settle(current)
                    }
                }

                override fun onMessage(webSocket: WebSocket, text: String) {
                    main.post {
                        if (!visible || sockets[id] !== webSocket) return@post
                        if (debugAuthObserverEnabled) {
                            PerfProbe.observeAuthenticatedMessage(id, webSocket, text, debugAuthObserverEnabled)
                        }
                        events?.success(mapOf("id" to id, "event" to "text", "data" to text))
                    }
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    main.post {
                        if (sockets.remove(id, webSocket)) {
                            pointer.drop(id)
                            PerfProbe.connectionEnded(id, webSocket, "closed")
                            releaseWifiIfIdle()
                            emit(id, "close")
                        }
                        settle(false)
                    }
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    main.post {
                        if (sockets.remove(id, webSocket)) {
                            pointer.drop(id)
                            PerfProbe.connectionEnded(id, webSocket, "failed")
                            releaseWifiIfIdle()
                            emit(id, "close")
                        }
                        settle(false)
                    }
                }
            },
        )
        sockets[id] = ws
        PerfProbe.registerConnection(id, ws)
    }

    private fun pointerSocketFactory(): SocketFactory {
        val wifi = wifiNetwork()
        return object : SocketFactory() {
            override fun createSocket(): Socket = RoutedSocket(wifi, ::wifiReaches)

            override fun createSocket(host: String?, port: Int): Socket =
                open(InetSocketAddress(host, port))

            override fun createSocket(
                host: String?,
                port: Int,
                localHost: InetAddress?,
                localPort: Int,
            ): Socket = createSocket(host, port)

            override fun createSocket(host: InetAddress?, port: Int): Socket =
                open(InetSocketAddress(host, port))

            override fun createSocket(
                address: InetAddress?,
                port: Int,
                localAddress: InetAddress?,
                localPort: Int,
            ): Socket = createSocket(address, port)

            private fun open(endpoint: InetSocketAddress): Socket {
                val s = RoutedSocket(wifi, ::wifiReaches)
                s.connect(endpoint)
                return s
            }
        }
    }

    /** True when [host] shares a prefix with the physical Wi‑Fi NIC. */
    private fun wifiReaches(host: InetAddress): Boolean {
        if (host.isLoopbackAddress) return false
        val wifi = wifiNetwork() ?: return false
        val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val lp = cm.getLinkProperties(wifi) ?: return false
        for (la in lp.linkAddresses) {
            val local = la.address
            if (local is Inet4Address && host is Inet4Address) {
                if (sameV4Prefix(local, host, la.prefixLength)) return true
            }
        }
        return false
    }

    private fun sameV4Prefix(a: Inet4Address, b: Inet4Address, prefix: Int): Boolean {
        if (prefix <= 0) return false
        val bits = prefix.coerceAtMost(32)
        val mask = if (bits == 0) 0 else -1 shl (32 - bits)
        fun toInt(x: Inet4Address): Int {
            val p = x.address
            return (p[0].toInt() and 0xff shl 24) or
                (p[1].toInt() and 0xff shl 16) or
                (p[2].toInt() and 0xff shl 8) or
                (p[3].toInt() and 0xff)
        }
        return (toInt(a) and mask) == (toInt(b) and mask)
    }

    // Peak supported rate — current getRefreshRate() is often 60 while Flutter is mode-locked.
    // Logical back camera's ratio range; CameraX switches physical lenses inside it.
    private fun cameraZoomRange(): List<Double> {
        val cm = ctx.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        val c = cm.cameraIdList.asSequence()
            .map { cm.getCameraCharacteristics(it) }
            .firstOrNull { it.get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_BACK }
            ?: return listOf(1.0, 1.0)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            c.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)?.let {
                return listOf(it.lower.toDouble(), it.upper.toDouble())
            }
        }
        return listOf(1.0, (c.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f).toDouble())
    }

    private fun displayRefreshHz(): Double {
        val display =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                ctx.display
            } else {
                @Suppress("DEPRECATION")
                (ctx.getSystemService(Context.WINDOW_SERVICE) as WindowManager).defaultDisplay
            } ?: return 0.0
        var peak = display.refreshRate.toDouble()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            for (mode in display.supportedModes) {
                peak = maxOf(peak, mode.refreshRate.toDouble())
            }
        }
        return peak
    }

    private fun holdWifi() {
        if (!visible) return
        wifiLock?.takeUnless { it.isHeld }?.let { runCatching { it.acquire() } }
        cpuLock?.takeUnless { it.isHeld }?.let { runCatching { it.acquire() } }
    }

    private fun releaseWifiIfIdle() {
        if (!sockets.isEmpty()) return
        runCatching { wifiLock?.takeIf { it.isHeld }?.release() }
        runCatching { cpuLock?.takeIf { it.isHeld }?.release() }
    }

    @Suppress("DEPRECATION")
    private fun newWifiLock(): WifiManager.WifiLock {
        val wifi = ctx.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val mode =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                WifiManager.WIFI_MODE_FULL_LOW_LATENCY
            } else {
                WifiManager.WIFI_MODE_FULL_HIGH_PERF
            }
        return wifi.createWifiLock(mode, "agentpad").apply { setReferenceCounted(false) }
    }

    private fun wifiNetwork(): Network? {
        val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        return cm.allNetworks.firstOrNull { n ->
            val caps = cm.getNetworkCapabilities(n) ?: return@firstOrNull false
            caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) &&
                caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
        }
    }
}

private fun num(call: MethodCall, key: String): Double {
    val v = call.argument<Any>(key) ?: return 0.0
    return if (v is Number) v.toDouble() else 0.0
}

private fun intNum(call: MethodCall, key: String): Int {
    val v = call.argument<Any>(key) ?: return 0
    return if (v is Number) v.toInt() else 0
}

internal object LauncherIcon {
    private const val PREFS = "agentpad_app_icon"
    private const val PENDING = "pending_app_icon"

    /** 仅由稳定 MainActivity 任务根在冷启动时应用待更新图标。 */
    fun onColdStart(activity: android.app.Activity) {
        applyPending(activity)
    }

    fun finish(activity: android.app.Activity) {
        applyPending(activity)
        activity.finish()
    }

    fun restart(activity: android.app.Activity) {
        val target = readPending(activity) ?: current(activity)
        savePending(activity, target)
        applyAliases(activity, target)
        // 先结束旧实例，避免 singleTop 将重启交回正在退出的窗口。
        activity.finishAndRemoveTask()
        activity.startActivity(Intent(activity, MainActivity::class.java).addFlags(
            Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK,
        ))
        clearPending(activity)
    }

    private fun applyPending(ctx: Context) {
        val pending = readPending(ctx) ?: return
        applyAliases(ctx, pending)
        clearPending(ctx)
    }

    /** 只记录待切换。返回值表示待切换与当前生效图标不同。 */
    fun setPending(ctx: Context, icon: String): Boolean {
        val target = if (icon == "black") "black" else "white"
        if (isApplied(ctx, target)) {
            clearPending(ctx)
            return false
        }
        savePending(ctx, target)
        return true
    }

    private fun savePending(ctx: Context, target: String) {
        val previous = readPending(ctx)
        if (!prefs(ctx).edit().putString(PENDING, target).commit()) {
            prefs(ctx).edit().putString(PENDING, previous).commit()
            error("无法保存图标设置")
        }
    }

    fun state(ctx: Context): Map<String, String?> {
        val now = current(ctx)
        val pending = readPending(ctx)
        return mapOf("current" to now, "pending" to pending)
    }

    private fun applyAliases(ctx: Context, target: String): ComponentName {
        val black = target == "black"
        val enable = component(ctx, if (black) "MainActivityBlack" else "MainActivityWhite")
        val disable = component(ctx, if (black) "MainActivityWhite" else "MainActivityBlack")
        setEnabled(ctx, enable, PackageManager.COMPONENT_ENABLED_STATE_ENABLED)
        setEnabled(ctx, disable, PackageManager.COMPONENT_ENABLED_STATE_DISABLED)
        restoreActivity(ctx)
        check(ctx.packageManager.getComponentEnabledSetting(enable) == PackageManager.COMPONENT_ENABLED_STATE_ENABLED &&
            ctx.packageManager.getComponentEnabledSetting(disable) == PackageManager.COMPONENT_ENABLED_STATE_DISABLED) {
            "图标切换未生效"
        }
        return enable
    }

    private fun restoreActivity(ctx: Context) {
        setEnabled(
            ctx,
            component(ctx, "MainActivity"),
            PackageManager.COMPONENT_ENABLED_STATE_DEFAULT,
        )
    }

    private fun setEnabled(ctx: Context, component: ComponentName, state: Int) {
        val pm = ctx.packageManager
        if (pm.getComponentEnabledSetting(component) == state) return
        pm.setComponentEnabledSetting(component, state, PackageManager.DONT_KILL_APP)
    }

    /** 黑图标仅当 Black alias 被显式 ENABLED。DEFAULT 按清单视为未启用。 */
    private fun current(ctx: Context): String {
        val state = ctx.packageManager.getComponentEnabledSetting(component(ctx, "MainActivityBlack"))
        return if (state == PackageManager.COMPONENT_ENABLED_STATE_ENABLED) "black" else "white"
    }

    private fun isApplied(ctx: Context, target: String): Boolean {
        fun enabled(name: String, default: Boolean): Boolean =
            when (ctx.packageManager.getComponentEnabledSetting(component(ctx, name))) {
                PackageManager.COMPONENT_ENABLED_STATE_ENABLED -> true
                PackageManager.COMPONENT_ENABLED_STATE_DEFAULT -> default
                else -> false
            }
        val black = enabled("MainActivityBlack", false)
        val white = enabled("MainActivityWhite", true)
        return if (target == "black") black && !white else white && !black
    }

    private fun readPending(ctx: Context): String? {
        val value = prefs(ctx).getString(PENDING, null)
        return if (value == "white" || value == "black") value else null
    }

    private fun clearPending(ctx: Context) {
        val previous = readPending(ctx)
        if (!prefs(ctx).edit().remove(PENDING).commit()) {
            prefs(ctx).edit().putString(PENDING, previous).commit()
            error("无法保存图标状态")
        }
    }

    private fun prefs(ctx: Context) =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun component(ctx: Context, simpleName: String) =
        ComponentName(ctx.packageName, "${MainActivity::class.java.packageName}.$simpleName")
}
