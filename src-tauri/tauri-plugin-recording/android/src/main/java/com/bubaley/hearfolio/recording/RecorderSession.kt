package com.bubaley.hearfolio.recording

import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AudioEffect
import android.media.audiofx.AutomaticGainControl
import android.os.Process
import android.util.Log
import java.io.File

// The foreground service owns capture. Activity and WebView lifetimes never
// close the input. The worker owns every read/replacement/release after startup.
internal class RecorderSession(private val onFailure: (Exception) -> Unit) {
    private var worker: Thread? = null
    private var loop: RecorderCaptureLoop? = null
    val level: Float get() = loop?.envelope?.level ?: 0f
    val elapsedMillis: Long get() = loop?.envelope?.elapsedMillis ?: 0L
    fun envelope(): RecorderEnvelope = loop?.envelope?.snapshot() ?: RecorderEnvelope(emptyList(), 0, 0)
    val isClosed: Boolean get() = worker?.isAlive != true

    fun start(file: File) {
        require(file.createNewFile())
        var configuration: RecorderCaptureConfiguration? = null
        var initial: RecorderInput? = null
        for (candidate in RecorderCapturePolicy.configurations) {
            try {
                initial = open(candidate); configuration = candidate; break
            } catch (error: Exception) {
                if (error is SecurityException) throw error
            }
        }
        val selected = requireNotNull(configuration)
        val input = requireNotNull(initial)
        val capture = RecorderCaptureLoop(file, selected.sampleRate, input, { open(selected) }, onFailure,
            { code -> Log.w("HearfolioRecording", "Audio input recreated; readCode=$code; sampleRate=${selected.sampleRate}") })
        loop = capture
        val thread = Thread({
            try { Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO) } catch(_: Exception) { }
            capture.run()
        }, "HearfolioAudioCapture")
        worker=thread
        try { thread.start() } catch(error:Exception) { capture.stop();input.close();loop=null;throw error }
    }

    @Suppress("DEPRECATION")
    private fun open(configuration: RecorderCaptureConfiguration): RecorderInput {
        var record: AudioRecord? = null
        val gain = RecorderGainControl(
            isAvailable = { AutomaticGainControl.isAvailable() },
            create = { session -> AutomaticGainControl.create(session)?.let { effect ->
                object : RecorderGainEffect {
                    override fun isEnabled(): Boolean = effect.enabled
                    override fun enable(): Boolean = effect.setEnabled(true) == AudioEffect.SUCCESS
                    override fun release() { effect.release() }
                }
            } }
        )
        try {
            val minimum = AudioRecord.getMinBufferSize(configuration.sampleRate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            require(minimum > 0)
            val active = AudioRecord(configuration.source, configuration.sampleRate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, maxOf(minimum * 4, configuration.sampleRate / 5 * 2))
            record = active
            require(active.state == AudioRecord.STATE_INITIALIZED)
            gain.attach(active.audioSessionId)
            active.startRecording()
            require(active.recordingState == AudioRecord.RECORDSTATE_RECORDING)
            val agc = gain.diagnostics
            val source = if(configuration.source == MediaRecorder.AudioSource.MIC) "MIC" else "DEFAULT"
            Log.i("HearfolioRecording", "source=$source; sampleRate=${configuration.sampleRate}; agcAvailable=${agc.available}; agcAttached=${agc.attached}; agcEnabled=${agc.enabled}; agcStatus=${agc.status}")
            return object : RecorderInput {
                private var closed = false
                override fun read(samples: ShortArray) = active.read(samples, 0, samples.size, AudioRecord.READ_NON_BLOCKING)
                override fun close() {
                    if(closed) return
                    closed = true
                    try { active.stop() } catch(_: Exception) { }
                    gain.release()
                    try { active.release() } catch(error:Exception) { Log.w("HearfolioRecording","Audio input release failed",error) }
                }
            }
        } catch(error: Exception) {
            gain.release();try { record?.release() } catch(_:Exception) { }; throw error
        }
    }

    fun pause() { requireNotNull(loop).pause(true) }
    fun resume() { requireNotNull(loop).pause(false) }
    fun stop(): Boolean {
        val capture = loop ?: return true
        capture.stop()
        try { worker?.join(2000) } catch(_:InterruptedException) { Thread.currentThread().interrupt();return false }
        // Never release an input while another thread is inside read or writing
        // its WAV. A delayed IO close remains owned by that worker.
        return worker?.isAlive != true && !capture.failed
    }
    fun release() { stop() }
}
