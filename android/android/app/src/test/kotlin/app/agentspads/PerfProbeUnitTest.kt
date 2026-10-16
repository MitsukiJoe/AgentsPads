package app.agentspads

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PerfProbeUnitTest {
    @Test
    fun boundedRingBufferKeepsNewestEventsAndCountsOverflow() {
        val buffer = BoundedRingBuffer<Int>(2)

        buffer.add(1)
        buffer.add(2)
        buffer.add(3)

        assertEquals(listOf(2, 3), buffer.snapshot())
        assertEquals(1L, buffer.droppedCount)
    }

    @Test
    fun authReadyRequiresDebugCurrentSocketAndConnectedMessage() {
        val connected = "{\"type\":\"connected\"}"
        assertFalse(PerfProbeAuthGate.isAuthenticatedConnectedMessage(false, true, connected))
        assertFalse(PerfProbeAuthGate.isAuthenticatedConnectedMessage(true, false, connected))
        assertFalse(PerfProbeAuthGate.isAuthenticatedConnectedMessage(true, true, "{\"type\":\"challenge\"}"))
        assertTrue(PerfProbeAuthGate.isAuthenticatedConnectedMessage(true, true, connected))
    }

    @Test
    fun probeRequiresBothDebugBuildAndMarker() {
        assertFalse(PerfProbeGate.shouldEnable(debugBuild = false, markerExists = true))
        assertFalse(PerfProbeGate.shouldEnable(debugBuild = true, markerExists = false))
        assertTrue(PerfProbeGate.shouldEnable(debugBuild = true, markerExists = true))
    }
}
