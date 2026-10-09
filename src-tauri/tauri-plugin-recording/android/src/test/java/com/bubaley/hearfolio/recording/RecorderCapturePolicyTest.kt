package com.bubaley.hearfolio.recording

import android.media.MediaRecorder
import org.junit.Assert.*
import org.junit.Test

class RecorderCapturePolicyTest {
    private class FakeEffect(var enabled: Boolean = false) : RecorderGainEffect {
        var enableCalls = 0
        var releaseCalls = 0
        var rejectEnable = false
        var throwOnEnable = false
        var throwOnRelease = false
        override fun isEnabled() = enabled
        override fun enable(): Boolean {
            enableCalls++
            if (throwOnEnable) throw IllegalStateException("Unsupported effect")
            if (rejectEnable) return false
            enabled = true
            return true
        }
        override fun release() {
            releaseCalls++
            if (throwOnRelease) throw IllegalStateException("Released native effect")
        }
    }

    @Test fun microphoneProcessingRemainsPreferredAcrossRateFallback() {
        val configurations = RecorderCapturePolicy.configurations
        assertEquals(listOf(MediaRecorder.AudioSource.MIC, MediaRecorder.AudioSource.MIC,
            MediaRecorder.AudioSource.DEFAULT, MediaRecorder.AudioSource.DEFAULT), configurations.map { it.source })
        assertEquals(listOf(48000, 44100, 48000, 44100), configurations.map { it.sampleRate })
        // On a device rejecting 48 kHz, stay on MIC instead of switching source.
        assertEquals(MediaRecorder.AudioSource.MIC, configurations.first { it.sampleRate == 44100 }.source)
    }

    @Test fun unavailableEffectDoesNotCreateOrPreventCapture() {
        var creations = 0
        val gain = RecorderGainControl({ false }, { creations++; null })
        assertEquals("unavailable", gain.attach(19).status)
        assertEquals(0, creations)
        gain.release()
    }

    @Test fun effectCreationMayReturnNullWithoutBreakingCapture() {
        val gain = RecorderGainControl({ true }, { null })
        val status = gain.attach(19)
        assertTrue(status.available)
        assertFalse(status.attached)
        assertEquals("create_unavailable", status.status)
    }

    @Test fun enabledOemEffectIsKeptWithoutTogglingAndReleasedExactlyOnce() {
        val effect = FakeEffect(enabled = true)
        var session = 0
        val gain = RecorderGainControl({ true }, { session = it; effect })
        assertEquals("already_enabled", gain.attach(73).status)
        assertEquals(73, session)
        assertEquals(0, effect.enableCalls)
        assertEquals(0, effect.releaseCalls)
        gain.release(); gain.release()
        assertEquals(1, effect.releaseCalls)
    }

    @Test fun disabledEffectIsEnabledOnTheCaptureSessionAndRetainedUntilCleanup() {
        val effect = FakeEffect()
        val gain = RecorderGainControl({ true }, { effect })
        val status = gain.attach(73)
        assertTrue(status.available && status.attached && status.enabled)
        assertEquals("enabled", status.status)
        assertEquals(1, effect.enableCalls)
        assertEquals(0, effect.releaseCalls)
        gain.release()
        assertEquals(1, effect.releaseCalls)
    }

    @Test fun rejectedEffectEnablingLeavesCaptureUsableAndIsReleased() {
        val effect = FakeEffect().also { it.rejectEnable = true }
        val gain = RecorderGainControl({ true }, { effect })
        val status = gain.attach(73)
        assertTrue(status.attached)
        assertFalse(status.enabled)
        assertEquals("enable_rejected", status.status)
        gain.release()
        assertEquals(1, effect.releaseCalls)
    }

    @Test fun brokenEffectApiOrCleanupDoesNotEscapeIntoCapture() {
        val effect = FakeEffect().also { it.throwOnEnable = true; it.throwOnRelease = true }
        val gain = RecorderGainControl({ true }, { effect })
        assertEquals("platform_error", gain.attach(73).status)
        assertEquals(1, effect.releaseCalls)
        gain.release()
        assertEquals(1, effect.releaseCalls)
        assertEquals("platform_error", RecorderGainControl({ throw IllegalStateException() }, { null }).attach(73).status)
        assertEquals("platform_error", RecorderGainControl({ true }, { throw IllegalStateException() }).attach(73).status)
    }

    @Test fun replacingSessionReleasesPreviousEffectAndNeverUsesGlobalSessionZero() {
        val old = FakeEffect()
        val next = FakeEffect()
        val sessions = mutableListOf<Int>()
        val gain = RecorderGainControl({ true }, { sessions.add(it); if (it == 73) old else next })
        gain.attach(73); gain.attach(74)
        assertEquals(1, old.releaseCalls)
        assertEquals(listOf(73, 74), sessions)
        gain.release()
        assertEquals(1, next.releaseCalls)
        assertEquals("invalid_session", gain.attach(0).status)
        assertEquals(listOf(73, 74), sessions)
    }
}
