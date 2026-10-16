package app.agentspads

import android.app.Activity
import android.app.ActivityManager
import android.app.UiAutomation
import android.os.Build
import android.os.Process
import android.util.DisplayMetrics
import android.view.InputDevice
import android.view.MotionEvent
import android.view.WindowManager
import java.io.File
import java.io.FileOutputStream
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import android.app.Instrumentation
import android.content.ComponentName
import android.content.Intent
import android.os.Bundle
import android.os.SystemClock
import android.view.KeyEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.ExtractedTextRequest
import android.view.inputmethod.InputConnection
import io.flutter.embedding.android.FlutterActivity

class InputConnectionChecks : Instrumentation() {
    private var checkIconRestart = false
    private var checkIconAliasLaunch = false
    private var checkTargetSwitchTouch = false
    private var checkPerfProbe = false
    private var targetSwitchArguments: Bundle? = null
    private val targetSwitchPointers = linkedMapOf<Int, ProbePoint>()
    private var targetSwitchDownTime = 0L
    private var targetSwitchLastEventTime = 0L
    private var targetSwitchAutomation: UiAutomation? = null
    private var targetSwitchEvents: File? = null
    private var targetSwitchTaskId = -1
    private var lastLaunch = ""

    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        checkIconRestart = arguments?.getString("check") == "icon-restart"
        checkIconAliasLaunch = arguments?.getString("check") == "icon-alias-launch"
        checkTargetSwitchTouch = arguments?.getString("check") == "target-switch-touch"
        checkPerfProbe = arguments?.getString("check") == "pointer-perf-probe"
        targetSwitchArguments = arguments
        start()
    }

    override fun onStart() {
        val result = Bundle()
        try {
            if (checkPerfProbe) {
                runPointerPerfProbe(targetSwitchArguments, result)
                return
            }
            if (checkTargetSwitchTouch) {
                runTargetSwitchTouchProbe(targetSwitchArguments, result)
                return
            }
            if (checkIconRestart) {
                targetContext.getSystemService(android.app.ActivityManager::class.java).appTasks
                    .forEach { it.finishAndRemoveTask() }
            }
            val activity = startActivitySync(
                Intent(targetContext, MainActivity::class.java)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            ) as MainActivity
            if (checkIconRestart) {
                val original = LauncherIcon.state(activity).getValue("current")!!
                val flutterIconPreference = saveFlutterIconPreference()
                var previous = activity
                for (target in listOf(if (original == "white") "black" else "white", original)) {
                    val oldTask = previous.taskId
                    val observer = object : ActivityMonitor() {
                        override fun onStartActivity(intent: Intent): ActivityResult? {
                            lastLaunch = "${intent.component}, flags=${intent.flags}"
                            return null
                        }
                    }
                    addMonitor(observer)
                    val monitor = addMonitor(MainActivity::class.java.name, null, false)
                    try {
                        runOnMainSync {
                            setFlutterIconPreference(target)
                            LauncherIcon.setPending(previous, target)
                            LauncherIcon.restart(previous)
                        }
                        val restarted = checkNotNull(waitForMonitorWithTimeout(monitor, 5000)) as MainActivity
                        check(restarted.taskId != oldTask) { "Restart reused the disabled launcher's task" }
                        SystemClock.sleep(1500)
                        check(!restarted.isFinishing && !restarted.isDestroyed) { "Restarted activity was closed" }
                        check(uiAutomation.rootInActiveWindow?.packageName == targetContext.packageName) {
                            "Restart did not retain the foreground window"
                        }
                        val tasks = targetContext.getSystemService(android.app.ActivityManager::class.java).appTasks
                        check(tasks.size == 1) { "Restart left ${tasks.size} app tasks" }
                        check(tasks.single().taskInfo.baseIntent.component?.className == MainActivity::class.java.name)
                        check(LauncherIcon.state(restarted).getValue("current") == target)
                        previous = restarted
                    } finally {
                        removeMonitor(monitor)
                        removeMonitor(observer)
                        restoreFlutterIconPreference(flutterIconPreference)
                    }
                }
                result.putString("stream", "OK: icon restart creates one visible task and retires the disabled alias (2 directions)\n")
                finish(Activity.RESULT_OK, result)
                return
            }
            if (checkIconAliasLaunch) {
                val original = LauncherIcon.state(activity).getValue("current")!!
                val target = if (original == "white") "black" else "white"
                val flutterIconPreference = saveFlutterIconPreference()
                val packageManager = targetContext.packageManager
                val activityManager = targetContext.getSystemService(ActivityManager::class.java)
                val oldTaskId = activity.taskId
                activityManager.appTasks
                    .filter { it.taskInfo.id != oldTaskId }
                    .forEach { it.finishAndRemoveTask() }
                SystemClock.sleep(1500)
                val launchIntent = checkNotNull(
                    packageManager.getLaunchIntentForPackage(targetContext.packageName)
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK)
                val launchAlias = launchIntent.component?.className.orEmpty()
                check(launchAlias.endsWith("MainActivity${original.replaceFirstChar { it.uppercase() }}")) {
                    "Expected the old $original alias to be the launcher entry, got $launchAlias"
                }
                var mainMonitor: ActivityMonitor? = null
                var observer: ActivityMonitor? = null
                var launched: MainActivity? = null
                try {
                    setFlutterIconPreference(target)
                    check(LauncherIcon.setPending(activity, target)) {
                        "Expected to save a pending icon change from $original to $target"
                    }
                    checkOnMain { check(activity.moveTaskToBack(true)) { "Could not move the original task to the background" } }
                    SystemClock.sleep(500)
                    check(uiAutomation.rootInActiveWindow?.packageName != targetContext.packageName) {
                        "The app did not leave the foreground before background recovery"
                    }
                    activityManager.appTasks.single().moveToFront()
                    SystemClock.sleep(500)
                    val resumedState = LauncherIcon.state(activity)
                    check(
                        resumedState.getValue("current") == original &&
                            resumedState.getValue("pending") == target
                    ) {
                        "Background recovery applied or lost the pending icon: $resumedState"
                    }
                    runOnMainSync { activity.finishAndRemoveTask() }
                    SystemClock.sleep(250)
                    check(activityManager.appTasks.none { it.taskInfo.id == oldTaskId }) {
                        "The old MainActivity task was not removed before alias launch"
                    }

                    val launchMonitor = addMonitor(MainActivity::class.java.name, null, false)
                    mainMonitor = launchMonitor
                    val launchObserver = object : ActivityMonitor() {
                        override fun onStartActivity(intent: Intent): ActivityResult? {
                            lastLaunch = "${intent.component}, flags=${intent.flags}"
                            return null
                        }
                    }
                    observer = launchObserver
                    addMonitor(launchObserver)
                    targetContext.startActivity(launchIntent)
                    val startedActivity =
                        checkNotNull(waitForMonitorWithTimeout(launchMonitor, 5000)) as MainActivity
                    check(startedActivity !== activity) {
                        "Alias launch monitor returned the old MainActivity instance"
                    }
                    check(startedActivity.taskId != oldTaskId) {
                        "Alias launch reused the old MainActivity task"
                    }
                    launched = startedActivity
                    SystemClock.sleep(1500)
                    val state = LauncherIcon.state(startedActivity)
                    val appliedIcon = state.getValue("current")
                    val pendingIcon = state.getValue("pending")
                    check(appliedIcon == target && pendingIcon == null) {
                        "Alias launch kept $appliedIcon with pending $pendingIcon; expected $target"
                    }
                    check(!startedActivity.isFinishing && !startedActivity.isDestroyed) {
                        "Applying a pending icon change closed the alias-launched activity"
                    }
                    check(uiAutomation.rootInActiveWindow?.packageName == targetContext.packageName) {
                        "Alias launch did not retain the app in the foreground"
                    }
                    check(launchMonitor.hits == 1) {
                        "Expected exactly one trampoline-to-MainActivity route"
                    }
                    val tasks = activityManager.appTasks
                    check(
                        tasks.size == 1 &&
                            tasks.single().taskInfo.id == startedActivity.taskId &&
                            tasks.none { it.taskInfo.id == oldTaskId }
                    ) {
                        "Alias launch did not retain only its new task; old=$oldTaskId, new=${startedActivity.taskId}, tasks=${tasks.map { it.taskInfo.id }}"
                    }
                    val taskInfo = tasks.single().taskInfo
                    check(
                        taskInfo.baseIntent.component?.className == MainActivity::class.java.name &&
                            taskInfo.baseActivity?.className == MainActivity::class.java.name,
                    ) {
                        "The stable MainActivity task root was lost after applying the pending icon"
                    }
                    result.putString(
                        "stream",
                        "OK: launcher trampoline applies pending $original->$target once and leaves MainActivity as the sole foreground task root\n",
                    )
                } finally {
                    launched?.let { runOnMainSync { it.finishAndRemoveTask() } }
                    mainMonitor?.let { removeMonitor(it) }
                    observer?.let { removeMonitor(it) }
                    restoreIconForTest(original)
                    restoreFlutterIconPreference(flutterIconPreference)
                }
                finish(Activity.RESULT_OK, result)
                return
            }
            val view = activity.findViewById<BackspaceFlutterView>(FlutterActivity.FLUTTER_VIEW_ID)
            uiAutomation
            var focusedEditor = false
            repeat(50) {
                if (!focusedEditor) {
                    checkOnMain {
                        val provider = view.accessibilityNodeProvider
                        // ponytail: this small screen fits 256 semantics IDs; use integration tests if it grows.
                        for (id in 0 until 256) {
                            if (provider?.createAccessibilityNodeInfo(id)?.isEditable == true) {
                                focusedEditor = provider.performAction(id, AccessibilityNodeInfo.ACTION_CLICK, null)
                                break
                            }
                        }
                    }
                    if (!focusedEditor) SystemClock.sleep(100)
                }
            }
            check(focusedEditor) { "Flutter text field did not become accessible" }
            var connection: InputConnection? = null
            repeat(50) {
                if (connection == null) {
                    runOnMainSync { connection = view.onCreateInputConnection(EditorInfo()) }
                    if (connection == null) SystemClock.sleep(100)
                }
            }
            val input = checkNotNull(connection) { "Flutter input connection was not created" }
            var forwarded = 0
            val original = view.onEmptyBackspace
            try {
                checkOnMain {
                    view.onEmptyBackspace = { forwarded++ }
                    val text = { input.getExtractedText(ExtractedTextRequest(), 0).text.toString() }
                    input.finishComposingText()
                    input.setSelection(0, text().length)
                    input.commitText("", 1)
                    check(text().isEmpty())
                    input.deleteSurroundingText(1, 0)
                    check(forwarded == 1) { "Empty IME deletion must forward once; got $forwarded" }
                    input.commitText("a", 1)
                    input.deleteSurroundingText(1, 0)
                    check(text().isEmpty() && forwarded == 1)
                    input.deleteSurroundingText(1, 0)
                    check(forwarded == 2)
                    input.commitText("😀", 1)
                    input.deleteSurroundingTextInCodePoints(1, 0)
                    check(text().isEmpty() && forwarded == 2)
                    input.deleteSurroundingTextInCodePoints(1, 0)
                    check(forwarded == 3)
                    input.deleteSurroundingText(0, 0)
                    input.deleteSurroundingText(0, 1)
                    check(forwarded == 3)
                    input.setComposingText("ni", 1)
                    check(text() == "ni")
                    input.deleteSurroundingText(1, 0)
                    check(forwarded == 3)
                    input.commitText("你", 1)
                    check(text() == "你")
                    input.finishComposingText()
                    input.setSelection(0, 1)
                    input.commitText("paste", 1)
                    check(text() == "paste" && forwarded == 3)
                    input.setSelection(0, 5)
                    input.commitText("", 1)
                    val now = SystemClock.uptimeMillis()
                    for (repeat in 0..2) {
                        input.sendKeyEvent(KeyEvent(now, now, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL, repeat))
                    }
                    input.sendKeyEvent(KeyEvent(now, now, KeyEvent.ACTION_UP, KeyEvent.KEYCODE_DEL, 0))
                    check(forwarded == 6 && text().isEmpty())
                }
            } finally {
                runOnMainSync { view.onEmptyBackspace = original }
            }
            result.putString("stream", "OK: real Flutter InputConnection; empty/last-char/codepoint/no-op/composing/selection/paste/repeat (8 checks)\n")
            finish(Activity.RESULT_OK, result)
        } catch (error: Throwable) {
            result.putString("stream", "FAILURE: ${error.stackTraceToString()}\nLast launch: $lastLaunch\n")
            finish(Activity.RESULT_CANCELED, result)
        }
    }


    private fun runPointerPerfProbe(arguments: Bundle?, result: Bundle) {
        var enabled = false
        var runActive = false
        var traceWritten = false
        var rootJson: org.json.JSONObject? = null
        var failure: Throwable? = null
        var inputStartNs = 0L
        var inputEndNs = 0L
        var gestureEndNs = 0L
        var injectedMoves = 0
        var plannedMoves = 0
        var durationMs = 0
        var stepMs = 0
        var marker: File? = null
        var trace: File? = null
        var upDelivered = false
        var gestureStartRecorded = false
        var gestureEndRecorded = false
        var readinessTimeoutMs = 45_000
        val readiness = PerfReadinessTiming()
        val idleDelaysMs = listOf(2_000, 5_000, 10_000)
        try {
            enabled = parseProbeBoolean(arguments, "probeEnabled")
            durationMs = probeArgument(arguments, "durationMs", 30_000)
            stepMs = probeArgument(arguments, "stepMs")
            readinessTimeoutMs = probeArgument(arguments, "readyTimeoutMs", 45_000)
            check(readinessTimeoutMs in 5_000..90_000) { "readyTimeoutMs must be between 5000 and 90000" }
            val metrics = DisplayMetrics()
            @Suppress("DEPRECATION")
            targetContext.getSystemService(WindowManager::class.java)
                .defaultDisplay.getRealMetrics(metrics)
            val center = probePoint(arguments, "padX", "padY", metrics.widthPixels, metrics.heightPixels)
            val amplitudePx = (10.0 / 0.14 * metrics.density).toInt().coerceAtLeast(1)
            val positiveEndpoint = ProbePoint(center.x + amplitudePx, center.y)
            check(durationMs in 1_000..30_000) { "durationMs must be between 1000 and 30000" }
            check(stepMs in setOf(8, 16, 33)) { "stepMs must be 8, 16, or 33" }
            check(positiveEndpoint.x < metrics.widthPixels - 1) {
                "padX leaves insufficient visible room for the fixed +10/-10 pointer movement"
            }
            plannedMoves = (durationMs / stepMs).let { if (it % 2 == 0) it else it - 1 }

            launchAndAwaitPerfReadiness(readinessTimeoutMs, readiness)
            val activeWindow = uiAutomation.rootInActiveWindow
            val activePackage = activeWindow?.packageName?.toString()
            activeWindow?.recycle()
            check(activePackage == targetContext.packageName) {
                "The target app must own the visible touchpad window"
            }

            marker = File(targetContext.filesDir, "perf-probe.enable")
            trace = File(targetContext.filesDir, "perf-probe.json")
            check(PerfProbe.hasOpenConnection() && PerfProbe.openConnectionCount() > 0) {
                "Connect and verify the app is online before the pressure gesture"
            }
            val drainDeadlineMs = SystemClock.uptimeMillis() + 2_000L
            var consecutiveDrainedSnapshots = 0
            while (consecutiveDrainedSnapshots < 2 && SystemClock.uptimeMillis() < drainDeadlineMs) {
                if (PerfProbe.queuesDrained()) {
                    consecutiveDrainedSnapshots++
                    SystemClock.sleep(100)
                } else {
                    consecutiveDrainedSnapshots = 0
                    SystemClock.sleep(25)
                }
            }
            check(consecutiveDrainedSnapshots == 2) { "Pointer and OkHttp queues must stay drained before the run starts" }
            if (enabled) {
                check(marker.isFile) { "Create files/perf-probe.enable before an enabled run" }
                check(!trace.exists()) { "Remove the prior files/perf-probe.json before an enabled run" }
                check(PerfProbe.beginMarkedRun(targetContext)) {
                    "Performance probe is unavailable outside a debuggable build with the marker"
                }
                runActive = true
            } else {
                check(!marker.exists()) { "Remove files/perf-probe.enable before the baseline run" }
                PerfProbe.refreshEnabled(targetContext)
                check(!trace.exists()) { "Remove the prior files/perf-probe.json before the baseline run" }
            }

            inputStartNs = SystemClock.elapsedRealtimeNanos()
            if (enabled) {
                PerfProbe.gestureBoundary(
                    sampleType = "gesture_start",
                    atNs = inputStartNs,
                    plannedDurationMs = durationMs.toLong(),
                    stepMs = stepMs.toLong(),
                    remoteAmplitudePerLeg = 10.0,
                    openConnectionCount = PerfProbe.openConnectionCount(),
                )
                gestureStartRecorded = true
            }

            var lastEventTimeMs = SystemClock.uptimeMillis() - 1
            val downTimeMs = lastEventTimeMs + 1
            var downAttempted = false
            try {
                downAttempted = true
                lastEventTimeMs = injectPerfMotion(MotionEvent.ACTION_DOWN, center, downTimeMs, lastEventTimeMs)
                val inputDeadlineNs = inputStartNs + durationMs * 1_000_000L
                for (index in 0 until plannedMoves) {
                    waitUntilElapsed(inputStartNs + (index + 1L) * stepMs * 1_000_000L)
                    val point = if (index % 2 == 0) positiveEndpoint else center
                    lastEventTimeMs = injectPerfMotion(MotionEvent.ACTION_MOVE, point, downTimeMs, lastEventTimeMs)
                    injectedMoves++
                }
                waitUntilElapsed(inputDeadlineNs)
            } catch (error: Throwable) {
                failure = error
            } finally {
                if (downAttempted) {
                    try {
                        lastEventTimeMs = injectPerfMotion(MotionEvent.ACTION_UP, center, downTimeMs, lastEventTimeMs)
                        upDelivered = true
                    } catch (error: Throwable) {
                        failure = failure ?: error
                        runCatching {
                            injectPerfMotion(MotionEvent.ACTION_CANCEL, center, downTimeMs, lastEventTimeMs)
                        }
                    }
                }
                inputEndNs = SystemClock.elapsedRealtimeNanos()
                SystemClock.sleep(250)
                gestureEndNs = SystemClock.elapsedRealtimeNanos()
                if (enabled && gestureStartRecorded) {
                    try {
                        PerfProbe.gestureBoundary(
                            sampleType = "gesture_end",
                            atNs = gestureEndNs,
                            plannedDurationMs = durationMs.toLong(),
                            stepMs = stepMs.toLong(),
                            remoteAmplitudePerLeg = 10.0,
                            openConnectionCount = PerfProbe.openConnectionCount(),
                            injectedMoves = injectedMoves.toLong(),
                            measuredGestureNs = (inputEndNs - inputStartNs).coerceAtLeast(0),
                            upDelivered = upDelivered,
                        )
                        gestureEndRecorded = true
                    } catch (error: Throwable) {
                        failure = failure ?: error
                    }
                }
            }

            for (delayMs in idleDelaysMs) {
                waitUntilElapsed(gestureEndNs + delayMs * 1_000_000L)
                if (enabled) {
                    try {
                        check(PerfProbe.recordIdleSnapshot() != null) { "Idle snapshot could not read live queues" }
                    } catch (error: Throwable) {
                        failure = failure ?: error
                    }
                }
            }

            if (enabled) {
                try {
                    rootJson = PerfProbe.snapshotAndWrite(targetContext)
                    check(rootJson != null) { "The opt-in performance trace could not be saved" }
                    traceWritten = true
                    if (failure == null) {
                        validatePointerPerfTrace(
                            checkNotNull(rootJson),
                            plannedMoves,
                            injectedMoves,
                            gestureStartRecorded,
                            gestureEndRecorded,
                            upDelivered,
                        )
                    }
                } catch (error: Throwable) {
                    failure = failure ?: error
                }
            } else {
                check(!checkNotNull(trace).exists()) { "The disabled baseline unexpectedly wrote a trace file" }
            }
        } catch (error: Throwable) {
            failure = failure ?: error
        } finally {
            if (enabled && runActive) {
                if (!traceWritten) {
                    runCatching {
                        rootJson = PerfProbe.snapshotAndWrite(targetContext)
                        traceWritten = rootJson != null
                    }.onFailure { error -> failure = failure ?: error }
                }
                PerfProbe.endMarkedRun()
                runActive = false
            }
        }

        val metadata = org.json.JSONObject()
            .put("mode", if (enabled) "probe_enabled" else "baseline_disabled")
            .put("planned_duration_ms", durationMs)
            .put("step_ms", stepMs)
            .put("planned_move_events", plannedMoves)
            .put("readiness_timeout_ms", readinessTimeoutMs)
            .put("readiness_status", readiness.status)
            .put("launcher_intent_resolved_elapsed_realtime_ns", readiness.launcherIntentResolvedNs)
            .put("activity_launch_started_elapsed_realtime_ns", readiness.activityLaunchStartedNs)
            .put("activity_launch_returned_elapsed_realtime_ns", readiness.activityLaunchReturnedNs)
            .put("main_activity_foreground_elapsed_realtime_ns", readiness.mainActivityForegroundNs)
            .put("authenticated_connected_elapsed_realtime_ns", readiness.authenticatedConnectedNs)
            .put("authenticated_connection_count_at_ready", readiness.authenticatedConnectionCount)
            .put("queues_drained_elapsed_realtime_ns", readiness.queuesDrainedNs)
            .put("readiness_wait_completed_elapsed_realtime_ns", readiness.waitCompletedNs)
            .put("launch_to_readiness_elapsed_ns", if (readiness.activityLaunchStartedNs > 0L && readiness.waitCompletedNs > 0L) readiness.waitCompletedNs - readiness.activityLaunchStartedNs else 0L)
            .put("injected_move_events", injectedMoves)
            .put("gesture_input_start_elapsed_realtime_ns", inputStartNs)
            .put("gesture_input_end_elapsed_realtime_ns", inputEndNs)
            .put("gesture_input_elapsed_ns", (inputEndNs - inputStartNs).coerceAtLeast(0))
            .put("input_duration_error_ns", inputEndNs - inputStartNs - durationMs * 1_000_000L)
            .put("time_error_definition", "elapsedRealtimeNanos immediately before ACTION_DOWN through immediately after final ACTION_UP; measured duration minus planned duration")
            .put("gesture_end_elapsed_realtime_ns", gestureEndNs)
            .put("remote_amplitude_per_leg", 10)
            .put("idle_snapshot_delays_ms", org.json.JSONArray(idleDelaysMs))
            .put("marker_enabled", marker?.isFile == true)
            .put("trace_written", traceWritten)
        metadata.put("run_id", if (enabled && rootJson != null) rootJson!!.getString("run_id") else java.util.UUID.randomUUID().toString())
        if (enabled && rootJson != null) {
            metadata.put("summary", rootJson!!.getJSONObject("summary"))
        }

        val resultCode = if (failure == null) Activity.RESULT_OK else Activity.RESULT_CANCELED
        val failureDetails = failure?.let {
            "\nFailure: " + it.javaClass.simpleName + ": " + (it.message ?: "no detail")
        }.orEmpty()
        result.putString("perf_probe_result", metadata.toString())
        result.putString(
            "stream",
            (if (resultCode == Activity.RESULT_OK) "OK" else "FAILURE") +
                ": pointer performance gesture; " + metadata + failureDetails + "\n",
        )
        finish(resultCode, result)
    }

    private fun parseProbeBoolean(arguments: Bundle?, name: String): Boolean =
        when (val raw = arguments?.get(name)) {
            is Boolean -> raw
            is String -> when (raw.lowercase()) {
                "true", "1" -> true
                "false", "0" -> false
                else -> error(name + " must be true or false")
            }
            else -> error("Missing boolean instrumentation argument: " + name)
        }

    private fun injectPerfMotion(
        action: Int,
        point: ProbePoint,
        downTimeMs: Long,
        previousEventTimeMs: Long,
    ): Long {
        val eventTimeMs = maxOf(SystemClock.uptimeMillis(), previousEventTimeMs + 1)
        val event = MotionEvent.obtain(downTimeMs, eventTimeMs, action, point.x, point.y, 0)
        return try {
            check(uiAutomation.injectInputEvent(event, true)) {
                "UiAutomation rejected pressure event action=" + action
            }
            eventTimeMs
        } finally {
            event.recycle()
        }
    }

    private fun waitUntilElapsed(deadlineNs: Long) {
        while (true) {
            val remainingNs = deadlineNs - SystemClock.elapsedRealtimeNanos()
            if (remainingNs <= 0) return
            val remainingMs = remainingNs / 1_000_000L
            if (remainingMs > 1) SystemClock.sleep(remainingMs.coerceAtMost(10))
            else SystemClock.sleep(1)
        }
    }

    private fun validatePointerPerfTrace(
        root: org.json.JSONObject,
        plannedMoves: Int,
        injectedMoves: Int,
        gestureStartRecorded: Boolean,
        gestureEndRecorded: Boolean,
        upDelivered: Boolean,
    ) {
        check(gestureStartRecorded && gestureEndRecorded && upDelivered) {
            "Trace must contain both pressure boundaries and a delivered final UP"
        }
        val events = root.getJSONArray("events")
        var startCount = 0
        var endCount = 0
        var enqueueCount = 0
        var sendCount = 0
        for (index in 0 until events.length()) {
            val sample = events.getJSONObject(index)
            when (sample.getString("sample_type")) {
                "gesture_start" -> startCount++
                "gesture_end" -> endCount++
                "enqueue" -> enqueueCount++
                "websocket_send" -> {
                    sendCount++
                    check(sample.getLong("enqueue_ns") <= sample.getLong("take_ns"))
                    check(sample.getLong("take_ns") <= sample.getLong("send_start_ns"))
                    check(sample.getLong("send_start_ns") <= sample.getLong("send_end_ns"))
                }
            }
        }
        check(startCount == 1 && endCount == 1) { "Expected one fixed start/end sample" }
        check(enqueueCount > 0 && sendCount > 0) { "Real app gesture produced no pointer samples" }
        val summary = root.getJSONObject("summary")
        val pointer = summary.getJSONObject("pointer_pump")
        val addCalls = pointer.getLong("add_calls")
        val created = pointer.getLong("segments_created")
        val coalesced = pointer.getLong("coalesced_add_calls")
        val sends = pointer.getLong("send_calls")
        val accepted = pointer.getLong("send_accepted")
        val rejected = pointer.getLong("send_rejected")
        val skipped = pointer.getLong("send_skipped_no_socket")
        val dropped = pointer.getLong("segments_dropped")
        val current = pointer.getLong("current_segments")
        check(addCalls == created + coalesced) { "Pointer add counter identity failed" }
        check(created == sends + skipped + dropped + current) { "Pointer segment accounting did not settle" }
        check(sends == accepted + rejected) { "WebSocket send return accounting failed" }
        check(accepted > 0L && rejected == 0L && skipped == 0L && dropped == 0L && current == 0L) {
            "Pointer path was rejected, skipped, dropped, or remained queued"
        }
        check(summary.getLong("overflow_dropped_events") == 0L) { "The bounded trace overflowed" }
        check(injectedMoves == plannedMoves) {
            "UiAutomation injected " + injectedMoves + " moves; expected " + plannedMoves
        }
        val okhttp = summary.getJSONObject("okhttp_application_queue")
        check(okhttp.getLong("current_queued_bytes") == 0L) {
            "OkHttp still reports application-queued bytes after the idle drain window"
        }
        check(okhttp.getInt("current_unavailable_queue_count") == 0) {
            "At least one OkHttp queueSize snapshot was unavailable"
        }
    }

    private class PerfReadinessTiming {
        var status = "not_started"
        var launcherIntentResolvedNs = 0L
        var activityLaunchStartedNs = 0L
        var activityLaunchReturnedNs = 0L
        var mainActivityForegroundNs = 0L
        var authenticatedConnectedNs = 0L
        var authenticatedConnectionCount = 0
        var queuesDrainedNs = 0L
        var waitCompletedNs = 0L
    }

    private fun launchAndAwaitPerfReadiness(timeoutMs: Int, timing: PerfReadinessTiming) {
        timing.status = "resolving_launcher"
        val launchIntent = targetContext.packageManager.getLaunchIntentForPackage(targetContext.packageName)
            ?: run {
                timing.status = "launcher_missing"
                error("The target app has no enabled launcher activity")
            }
        timing.launcherIntentResolvedNs = SystemClock.elapsedRealtimeNanos()
        launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        timing.activityLaunchStartedNs = SystemClock.elapsedRealtimeNanos()
        timing.status = "launch_requested"
        startActivitySync(launchIntent)
        timing.activityLaunchReturnedNs = SystemClock.elapsedRealtimeNanos()
        timing.status = "waiting_for_foreground"

        val deadlineNs = timing.activityLaunchReturnedNs + timeoutMs * 1_000_000L
        var consecutiveDrainedSnapshots = 0
        while (SystemClock.elapsedRealtimeNanos() < deadlineNs) {
            val nowNs = SystemClock.elapsedRealtimeNanos()
            val foreground = runCatching {
                requireResumedMainActivity()
                true
            }.getOrDefault(false)
            if (foreground) {
                if (timing.mainActivityForegroundNs == 0L) timing.mainActivityForegroundNs = nowNs
                if (PerfProbe.hasAuthenticatedConnection()) {
                    if (timing.authenticatedConnectedNs == 0L) timing.authenticatedConnectedNs = nowNs
                    if (timing.authenticatedConnectionCount == 0) {
                        timing.authenticatedConnectionCount = PerfProbe.authenticatedConnectionCount()
                    }
                    timing.status = "waiting_for_drained_queues"
                    if (PerfProbe.queuesDrained()) {
                        consecutiveDrainedSnapshots++
                        if (consecutiveDrainedSnapshots >= 2) {
                            timing.queuesDrainedNs = nowNs
                            timing.waitCompletedNs = nowNs
                            timing.status = "ready"
                            return
                        }
                        SystemClock.sleep(100)
                    } else {
                        consecutiveDrainedSnapshots = 0
                        SystemClock.sleep(25)
                    }
                } else {
                    consecutiveDrainedSnapshots = 0
                    timing.status = "waiting_for_authenticated_connected"
                    SystemClock.sleep(100)
                }
            } else {
                consecutiveDrainedSnapshots = 0
                timing.status = "waiting_for_foreground"
                SystemClock.sleep(100)
            }
        }
        timing.waitCompletedNs = SystemClock.elapsedRealtimeNanos()
        error("Timed out waiting for launcher foreground, authenticated connected socket, and drained queues; last stage=" + timing.status)
    }

    private data class ProbePoint(val x: Float, val y: Float)

    private fun runTargetSwitchTouchProbe(arguments: Bundle?, result: Bundle) {
        val directory = targetSwitchProtocolDirectory()
        var resultCode = Activity.RESULT_CANCELED
        var status = "TRACE_ONLY_ABORTED"
        var details = "Probe did not reach its first phase"
        try {
            check(Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                "Requires Android 12+ so am instrument --no-restart can preserve the existing process"
            }

            clearTargetSwitchProtocol(directory)
            targetSwitchPointers.clear()
            targetSwitchDownTime = 0L
            targetSwitchLastEventTime = 0L
            targetSwitchAutomation = uiAutomation
            targetSwitchEvents = File(directory, "events.log")
            targetSwitchEvents!!.writeText("", Charsets.UTF_8)
            File(directory, "ui-phases.log").writeText("", Charsets.UTF_8)

            val waitSeconds = probeArgument(arguments, "waitSeconds", 60)
            check(waitSeconds == 30 || waitSeconds == 60) {
                "waitSeconds must be 30 or 60"
            }

            val metrics = DisplayMetrics()
            @Suppress("DEPRECATION")
            targetContext.getSystemService(WindowManager::class.java)
                .defaultDisplay.getRealMetrics(metrics)
            val suppliedWidth = probeArgument(arguments, "displayWidth")
            val suppliedHeight = probeArgument(arguments, "displayHeight")
            check(suppliedWidth == metrics.widthPixels && suppliedHeight == metrics.heightPixels) {
                "Root-observed display dimensions do not match the active display"
            }
            val pad = probePoint(arguments, "padX", "padY", metrics.widthPixels, metrics.heightPixels)
            val drag = probePoint(arguments, "dragX", "dragY", metrics.widthPixels, metrics.heightPixels)
            val header = probePoint(arguments, "headerX", "headerY", metrics.widthPixels, metrics.heightPixels)
            val checkbox = probePoint(arguments, "checkboxX", "checkboxY", metrics.widthPixels, metrics.heightPixels)
            val dx = drag.x - pad.x
            val dy = drag.y - pad.y
            val minimumDrag = metrics.density * 12f
            check(dx * dx + dy * dy > minimumDrag * minimumDrag) {
                "The drag coordinates must exceed the touchpad long-press movement threshold"
            }

            val taskId = requireResumedMainActivity()
            targetSwitchTaskId = taskId
            val info = buildString {
                append("sdk=").append(Build.VERSION.SDK_INT).append('\n')
                append("pid=").append(Process.myPid()).append('\n')
                append("targetPackage=").append(targetContext.packageName).append('\n')
                append("testPackage=").append(context.packageName).append('\n')
                append("taskId=").append(taskId).append('\n')
                append("display=").append(metrics.widthPixels).append('x').append(metrics.heightPixels).append('\n')
                append("cacheDir=").append(directory.absolutePath).append('\n')
            }
            File(directory, "info.txt").writeText(info, Charsets.UTF_8)
            recordTargetSwitchEvent("preflight task=" + taskId)
            awaitTargetSwitchPhase(directory, "rootReady", waitSeconds)

            requireResumedMainActivity()
            targetSwitchPointerDown(0, pad)
            SystemClock.sleep(700)
            targetSwitchPointerMove(0, drag)
            SystemClock.sleep(250)
            awaitTargetSwitchPhase(directory, "hold", waitSeconds)

            requireResumedMainActivity()
            targetSwitchTap(1, header)
            awaitTargetSwitchPhase(directory, "header", waitSeconds)

            requireResumedMainActivity()
            targetSwitchTap(1, checkbox)
            awaitTargetSwitchPhase(directory, "checkbox", waitSeconds)

            requireResumedMainActivity()
            val releaseErrors = releaseTargetSwitchPointers("explicit-up-before-finish")
            check(releaseErrors.isEmpty()) {
                "Could not inject every explicit UP event: " + releaseErrors.joinToString("; ")
            }
            check(targetSwitchPointers.isEmpty()) { "Pointers remained active after explicit UP" }
            awaitTargetSwitchPhase(directory, "finish", waitSeconds)

            status = "TRACE_ONLY"
            details =
                "Protocol phases completed; no UI, connection, target-selection, or GetAsyncKeyState assertion was made"
            resultCode = Activity.RESULT_OK
        } catch (error: Throwable) {
            status = "TRACE_ONLY_ABORTED"
            details = error.javaClass.simpleName + ": " + (error.message ?: "no detail")
            try {
                recordTargetSwitchEvent("abort " + details)
            } catch (_: Throwable) {
            }
        } finally {
            val cleanupErrors = targetSwitchAutomation?.let {
                releaseTargetSwitchPointers("finally-up")
            }.orEmpty()
            if (cleanupErrors.isNotEmpty()) {
                try {
                    recordTargetSwitchEvent(
                        "finally-release-errors " + cleanupErrors.joinToString("; "),
                    )
                } catch (_: Throwable) {
                }
                status = "TRACE_ONLY_ABORTED"
                details = details + "; final UP errors: " + cleanupErrors.joinToString("; ")
                resultCode = Activity.RESULT_CANCELED
            }
            targetSwitchAutomation = null
            targetSwitchEvents = null
        }

        result.putString(
            "stream",
            status + ": " + details +
                ". All observations must be completed during the finish phase; finishInstrumentation may force-stop the target package as teardown.",
        )
        finish(resultCode, result)
    }

    private fun targetSwitchProtocolDirectory(): File {
        val cacheRoot = targetContext.cacheDir.canonicalFile
        check(cacheRoot.isDirectory) { "Target app cache root is unavailable" }
        val directory = File(cacheRoot, TARGET_SWITCH_CACHE_DIRECTORY)
        val path = directory.toPath()
        check(!Files.isSymbolicLink(path)) {
            "Target switch protocol directory must not be a symbolic link"
        }
        if (directory.exists()) {
            check(directory.isDirectory) {
                "Target switch protocol path exists but is not a directory"
            }
        } else {
            check(directory.mkdir()) {
                "Could not create the target app cache protocol directory"
            }
        }

        val canonicalDirectory = directory.canonicalFile
        check(
            canonicalDirectory.parentFile == cacheRoot &&
                canonicalDirectory.name == TARGET_SWITCH_CACHE_DIRECTORY,
        ) {
            "Target switch protocol directory escaped its fixed cache child"
        }
        val names = targetSwitchProtocolNames().toSet()
        val entries = directory.list()?.toList()
            ?: error("Could not inspect the target switch protocol directory")
        check(entries.all { it in names }) {
            "Unexpected entry in the target switch protocol directory"
        }
        for (name in entries) {
            val entry = File(directory, name)
            check(!Files.isSymbolicLink(entry.toPath()) && entry.isFile) {
                "Abnormal existing protocol entry"
            }
        }
        return canonicalDirectory
    }

    private fun targetSwitchProtocolNames(): List<String> = listOf(
        "phase.txt",
        "phase.tmp",
        "info.txt",
        "events.log",
        "ui-phases.log",
        "continue-rootReady",
        "continue-hold",
        "continue-header",
        "continue-checkbox",
        "continue-finish",
    )

    private fun clearTargetSwitchProtocol(directory: File) {
        for (name in targetSwitchProtocolNames()) {
            val file = File(directory, name)
            check(!file.exists() || file.delete()) {
                "Could not reset the target app cache protocol file " + name
            }
        }
    }

    private fun probeArgument(arguments: Bundle?, name: String, defaultValue: Int? = null): Int {
        val raw = arguments?.get(name)
        val value = when (raw) {
            is Int -> raw
            is String -> raw.toIntOrNull()
            else -> null
        }
        return value ?: defaultValue ?: error("Missing integer instrumentation argument: " + name)
    }

    private fun probePoint(
        arguments: Bundle?,
        xName: String,
        yName: String,
        width: Int,
        height: Int,
    ): ProbePoint {
        val x = probeArgument(arguments, xName)
        val y = probeArgument(arguments, yName)
        check(x in 1 until width && y in 1 until height) {
            "Coordinate arguments " + xName + "/" + yName +
                " must be positive and inside the observed display"
        }
        return ProbePoint(x.toFloat(), y.toFloat())
    }

    private fun requireResumedMainActivity(): Int {
        val packageName = targetContext.packageName
        val activityManager = targetContext.getSystemService(ActivityManager::class.java)
        val process = activityManager.runningAppProcesses
            ?.firstOrNull { it.pid == Process.myPid() }
            ?: error("Instrumentation process is not listed by ActivityManager")
        check(process.processName == packageName) {
            "Instrumentation is not attached to the target app process"
        }
        check(process.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND) {
            "Target process is not foreground"
        }

        @Suppress("DEPRECATION")
        val topTask = activityManager.getRunningTasks(1).firstOrNull()
            ?: error("Could not inspect the foreground task")
        val topActivity = topTask.topActivity
        check(
            topActivity != null &&
                topActivity.packageName == packageName &&
                topActivity.className == MainActivity::class.java.name,
        ) {
            "Existing foreground task is not MainActivity"
        }
        val appTask = activityManager.appTasks.firstOrNull {
            it.taskInfo.id == topTask.taskId
        }
        check(appTask?.taskInfo?.baseActivity?.className == MainActivity::class.java.name) {
            "MainActivity task root could not be verified"
        }

        val root = uiAutomation.rootInActiveWindow
        val activePackage = root?.packageName?.toString()
        root?.recycle()
        check(activePackage == packageName) {
            "The target app does not own the active accessibility window"
        }
        return topTask.taskId
    }

    private fun awaitTargetSwitchPhase(directory: File, phase: String, waitSeconds: Int) {
        val signal = File(directory, "continue-" + phase)
        check(!signal.exists() || signal.delete()) {
            "Could not clear the prior " + phase + " continuation"
        }
        writeTargetSwitchPhase(directory, phase)
        val deadline = SystemClock.elapsedRealtime() + waitSeconds * 1000L
        while (SystemClock.elapsedRealtime() < deadline) {
            if (signal.isFile && signal.readText(Charsets.UTF_8).trim() == "continue") {
                check(signal.delete()) { "Could not consume the " + phase + " continuation" }
                recordTargetSwitchEvent("continue " + phase)
                return
            }
            SystemClock.sleep(100)
        }
        error("Timed out waiting for root continuation at phase " + phase)
    }

    private fun writeTargetSwitchPhase(directory: File, phase: String) {
        val target = File(directory, "phase.txt")
        val temporary = File(directory, "phase.tmp")
        temporary.writeText(
            phase + "\t" + SystemClock.uptimeMillis() + "\n",
            Charsets.UTF_8,
        )
        try {
            Files.move(
                temporary.toPath(),
                target.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
        } catch (_: AtomicMoveNotSupportedException) {
            Files.move(
                temporary.toPath(),
                target.toPath(),
                StandardCopyOption.REPLACE_EXISTING,
            )
        }
        recordTargetSwitchEvent("phase " + phase)
        recordTargetSwitchUiSnapshot(directory, phase)
    }

    private fun recordTargetSwitchUiSnapshot(directory: File, phase: String) {
        val file = File(directory, "ui-phases.log")
        val prefix = "phase=" + phase +
            "\tpid=" + Process.myPid() +
            "\ttask=" + targetSwitchTaskId +
            "\t"
        val lines = mutableListOf<String>()
        try {
            val automation = targetSwitchAutomation ?: error("UiAutomation unavailable")
            val root = automation.rootInActiveWindow
            if (root == null) {
                lines += prefix + "window=unavailable"
            } else {
                val pending = java.util.ArrayDeque<AccessibilityNodeInfo>()
                pending.addLast(root)
                var matches = 0
                try {
                    while (pending.isNotEmpty()) {
                        val node = pending.removeFirst()
                        try {
                            val className = node.className?.toString().orEmpty()
                            val checkable = node.isCheckable
                            if (checkable || className.endsWith("CheckBox", ignoreCase = true)) {
                                val bounds = android.graphics.Rect()
                                node.getBoundsInScreen(bounds)
                                lines += prefix + "class=" + className +
                                    "\tbounds=" + bounds.left + "," + bounds.top +
                                    "," + bounds.right + "," + bounds.bottom +
                                    "\tchecked=" + node.isChecked +
                                    "\tselected=" + node.isSelected
                                matches++
                            }
                            for (index in 0 until node.childCount) {
                                node.getChild(index)?.let(pending::addLast)
                            }
                        } finally {
                            node.recycle()
                        }
                    }
                } finally {
                    while (pending.isNotEmpty()) {
                        try {
                            pending.removeFirst().recycle()
                        } catch (_: Throwable) {
                        }
                    }
                }
                if (matches == 0) {
                    lines += prefix + "matches=0"
                }
            }
        } catch (error: Throwable) {
            lines.clear()
            lines += prefix + "snapshot-error=" + error.javaClass.simpleName
        }

        try {
            if (lines.isNotEmpty()) {
                FileOutputStream(file, true).use { output ->
                    output.write(
                        (lines.joinToString("\n") + "\n").toByteArray(Charsets.UTF_8),
                    )
                    output.fd.sync()
                }
            }
        } catch (_: Throwable) {
        }
    }

    private fun recordTargetSwitchEvent(event: String) {
        val file = targetSwitchEvents ?: return
        FileOutputStream(file, true).use {
            it.write(
                (SystemClock.uptimeMillis().toString() + "\t" + event + "\n")
                    .toByteArray(Charsets.UTF_8),
            )
            it.fd.sync()
        }
    }

    private fun targetSwitchTap(pointerId: Int, point: ProbePoint) {
        targetSwitchPointerDown(pointerId, point)
        SystemClock.sleep(90)
        targetSwitchPointerUp(pointerId)
        SystemClock.sleep(350)
    }

    private fun targetSwitchPointerDown(pointerId: Int, point: ProbePoint) {
        check(pointerId !in targetSwitchPointers) { "Pointer " + pointerId + " is already down" }
        if (targetSwitchPointers.isEmpty()) {
            targetSwitchDownTime = SystemClock.uptimeMillis()
            targetSwitchLastEventTime = targetSwitchDownTime - 1
        }
        targetSwitchPointers[pointerId] = point
        val ids = targetSwitchPointers.keys.sorted()
        val index = ids.indexOf(pointerId)
        val action = if (ids.size == 1) {
            MotionEvent.ACTION_DOWN
        } else {
            MotionEvent.ACTION_POINTER_DOWN or
                (index shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
        }
        injectTargetSwitchMotion(action, "down-" + pointerId)
    }

    private fun targetSwitchPointerMove(pointerId: Int, point: ProbePoint) {
        check(pointerId in targetSwitchPointers) { "Pointer " + pointerId + " is not down" }
        targetSwitchPointers[pointerId] = point
        injectTargetSwitchMotion(MotionEvent.ACTION_MOVE, "move-" + pointerId)
    }

    private fun targetSwitchPointerUp(pointerId: Int) {
        check(pointerId in targetSwitchPointers) { "Pointer " + pointerId + " is not down" }
        val ids = targetSwitchPointers.keys.sorted()
        val index = ids.indexOf(pointerId)
        val action = if (ids.size == 1) {
            MotionEvent.ACTION_UP
        } else {
            MotionEvent.ACTION_POINTER_UP or
                (index shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
        }
        injectTargetSwitchMotion(action, "up-" + pointerId)
        targetSwitchPointers.remove(pointerId)
    }

    private fun injectTargetSwitchMotion(action: Int, label: String) {
        val automation = checkNotNull(targetSwitchAutomation)
        val ordered = targetSwitchPointers.toSortedMap().entries.toList()
        check(ordered.isNotEmpty()) { "Cannot inject a touch event without active pointers" }
        val properties = Array(ordered.size) { MotionEvent.PointerProperties() }
        val coordinates = Array(ordered.size) { MotionEvent.PointerCoords() }
        ordered.forEachIndexed { index, entry ->
            properties[index].id = entry.key
            properties[index].toolType = MotionEvent.TOOL_TYPE_FINGER
            coordinates[index].x = entry.value.x
            coordinates[index].y = entry.value.y
            coordinates[index].pressure = 1f
            coordinates[index].size = 1f
        }
        val eventTime = maxOf(SystemClock.uptimeMillis(), targetSwitchLastEventTime + 1)
        val event = MotionEvent.obtain(
            targetSwitchDownTime,
            eventTime,
            action,
            properties.size,
            properties,
            coordinates,
            0,
            0,
            1f,
            1f,
            0,
            0,
            InputDevice.SOURCE_TOUCHSCREEN,
            0,
        )
        try {
            check(automation.injectInputEvent(event, true)) {
                "UiAutomation rejected touch event " + label
            }
            targetSwitchLastEventTime = eventTime
            recordTargetSwitchEvent("inject " + label + " eventTime=" + eventTime + " action=" + action)
        } finally {
            event.recycle()
        }
    }

    private fun releaseTargetSwitchPointers(reason: String): List<String> {
        val errors = mutableListOf<String>()
        for (pointerId in targetSwitchPointers.keys.sortedDescending()) {
            if (pointerId !in targetSwitchPointers) continue
            try {
                targetSwitchPointerUp(pointerId)
                recordTargetSwitchEvent(reason + " pointer=" + pointerId)
            } catch (error: Throwable) {
                errors += "pointer " + pointerId + ": " +
                    (error.message ?: error.javaClass.simpleName)
            }
        }
        if (targetSwitchPointers.isNotEmpty()) {
            try {
                injectTargetSwitchMotion(MotionEvent.ACTION_CANCEL, "cancel-fallback")
                targetSwitchPointers.clear()
                recordTargetSwitchEvent(reason + " cancel-fallback")
            } catch (error: Throwable) {
                errors += "cancel fallback: " + (error.message ?: error.javaClass.simpleName)
            }
        }
        return errors
    }
    private fun saveFlutterIconPreference(): Pair<Boolean, String?> {
        val preferences = targetContext.getSharedPreferences(
            FLUTTER_PREFERENCES,
            android.content.Context.MODE_PRIVATE,
        )
        return preferences.contains(FLUTTER_APP_ICON_KEY) to
            preferences.getString(FLUTTER_APP_ICON_KEY, null)
    }

    private fun setFlutterIconPreference(icon: String) {
        val preferences = targetContext.getSharedPreferences(
            FLUTTER_PREFERENCES,
            android.content.Context.MODE_PRIVATE,
        )
        check(preferences.edit().putString(FLUTTER_APP_ICON_KEY, icon).commit()) {
            "Could not persist the Flutter app icon fixture"
        }
    }

    private fun restoreFlutterIconPreference(preference: Pair<Boolean, String?>) {
        val preferences = targetContext.getSharedPreferences(
            FLUTTER_PREFERENCES,
            android.content.Context.MODE_PRIVATE,
        )
        val editor = preferences.edit()
        if (preference.first) {
            editor.putString(FLUTTER_APP_ICON_KEY, checkNotNull(preference.second))
        } else {
            editor.remove(FLUTTER_APP_ICON_KEY)
        }
        check(editor.commit()) { "Could not restore the Flutter app icon preference" }
    }
    private fun restoreIconForTest(icon: String) {
        val packageName = targetContext.packageName
        val namespace = MainActivity::class.java.packageName
        val enableName = if (icon == "black") "MainActivityBlack" else "MainActivityWhite"
        val disableName = if (icon == "black") "MainActivityWhite" else "MainActivityBlack"
        val packageManager = targetContext.packageManager
        packageManager.setComponentEnabledSetting(
            ComponentName(packageName, "$namespace.$enableName"),
            android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_ENABLED,
            android.content.pm.PackageManager.DONT_KILL_APP,
        )
        packageManager.setComponentEnabledSetting(
            ComponentName(packageName, "$namespace.$disableName"),
            android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_DISABLED,
            android.content.pm.PackageManager.DONT_KILL_APP,
        )
        packageManager.setComponentEnabledSetting(
            ComponentName(packageName, "$namespace.MainActivity"),
            android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_DEFAULT,
            android.content.pm.PackageManager.DONT_KILL_APP,
        )
        targetContext.getSharedPreferences("agentpad_app_icon", android.content.Context.MODE_PRIVATE)
            .edit().remove("pending_app_icon").commit()
    }

    private companion object {
        const val TARGET_SWITCH_CACHE_DIRECTORY = "agentspads-target-switch-touch"
        const val FLUTTER_PREFERENCES = "FlutterSharedPreferences"
        const val FLUTTER_APP_ICON_KEY = "flutter.app_icon"
    }

    private fun checkOnMain(block: () -> Unit) {
        var failure: Throwable? = null
        runOnMainSync {
            try {
                block()
            } catch (error: Throwable) {
                failure = error
            }
        }
        failure?.let { throw it }
    }
}
