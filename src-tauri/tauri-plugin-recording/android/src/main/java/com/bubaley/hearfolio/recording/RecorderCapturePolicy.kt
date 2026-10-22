package com.bubaley.hearfolio.recording

import android.media.MediaRecorder

internal data class RecorderCaptureConfiguration(val source: Int, val sampleRate: Int)

internal object RecorderCapturePolicy {
    // A voice recorder needs the OEM microphone processing path. Raw and ASR
    // sources intentionally bypass gain processing needed for speech at a desk.
    // 16 kHz carries the full speech band at a third of the 48 kHz PCM size;
    // the platform resamples from the hardware rate. 48/44.1 kHz remain
    // fallbacks for devices that reject it (44.1 kHz is the only guaranteed rate).
    val rates = listOf(16000, 48000, 44100)
    val configurations: List<RecorderCaptureConfiguration> =
        listOf(MediaRecorder.AudioSource.MIC, MediaRecorder.AudioSource.DEFAULT).flatMap { source ->
            rates.map { rate -> RecorderCaptureConfiguration(source, rate) }
        }
}

internal interface RecorderGainEffect {
    fun isEnabled(): Boolean
    fun enable(): Boolean
    fun release()
}

internal data class RecorderGainDiagnostics(
    val available: Boolean = false,
    val attached: Boolean = false,
    val enabled: Boolean = false,
    val status: String = "unavailable"
)

// The effect is optional: unsupported APIs, null creation, or rejected enabling
// never prevent recording. Ownership lasts until this capture session ends.
internal class RecorderGainControl(
    private val isAvailable: () -> Boolean,
    private val create: (Int) -> RecorderGainEffect?
) {
    private var effect: RecorderGainEffect? = null
    var diagnostics = RecorderGainDiagnostics()
        private set

    fun attach(audioSessionId: Int): RecorderGainDiagnostics {
        release()
        diagnostics = RecorderGainDiagnostics()
        try {
            if (!isAvailable()) return diagnostics
            diagnostics = RecorderGainDiagnostics(available = true)
            if (audioSessionId <= 0) {
                diagnostics = diagnostics.copy(status = "invalid_session")
                return diagnostics
            }
            effect = create(audioSessionId)
            val active = effect ?: run {
                diagnostics = diagnostics.copy(status = "create_unavailable")
                return diagnostics
            }
            diagnostics = diagnostics.copy(attached = true)
            if (active.isEnabled()) {
                diagnostics = diagnostics.copy(enabled = true, status = "already_enabled")
            } else {
                val accepted = active.enable()
                val enabled = active.isEnabled()
                diagnostics = diagnostics.copy(enabled = enabled, status = if (accepted && enabled) "enabled" else "enable_rejected")
            }
        } catch (_: Exception) {
            release()
            diagnostics = diagnostics.copy(attached = false, enabled = false, status = "platform_error")
        }
        return diagnostics
    }

    fun release() {
        val active = effect
        effect = null
        try { active?.release() } catch (_: Exception) { }
    }
}
