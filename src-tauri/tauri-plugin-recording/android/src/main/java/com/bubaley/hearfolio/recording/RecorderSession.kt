package com.bubaley.hearfolio.recording

import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AudioEffect
import android.media.audiofx.AutomaticGainControl
import android.util.Log
import java.io.File
import java.io.RandomAccessFile
import kotlin.math.log10
import kotlin.math.sqrt

// The foreground service owns the processed capture path from main. Paused
// samples are discarded, so WAV duration counts audio rather than wall time.
internal class RecorderSession(private val onFailure: () -> Unit) {
    private var recorder: AudioRecord? = null
    private var gainControl: RecorderGainControl? = null
    private var worker: Thread? = null
    @Volatile private var capturing = false
    @Volatile private var paused = false
    @Volatile private var failed = false
    @Volatile var level = 0f
        private set
    @Suppress("DEPRECATION")
    fun start(file: File) {
        require(file.createNewFile())
        var rate = 0
        var source = MediaRecorder.AudioSource.MIC
        for (configuration in RecorderCapturePolicy.configurations) {
            var candidate: AudioRecord? = null
            var candidateGain: RecorderGainControl? = null
            try {
                val minimum = AudioRecord.getMinBufferSize(configuration.sampleRate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
                require(minimum > 0)
                candidate = AudioRecord(configuration.source, configuration.sampleRate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, maxOf(minimum * 4, configuration.sampleRate / 5 * 2))
                require(candidate.state == AudioRecord.STATE_INITIALIZED)
                candidateGain = createGainControl()
                candidateGain.attach(candidate.audioSessionId)
                candidate.startRecording()
                require(candidate.recordingState == AudioRecord.RECORDSTATE_RECORDING)
                recorder = candidate; gainControl = candidateGain
                rate = configuration.sampleRate; source = configuration.source
                break
            } catch (error: Exception) {
                candidateGain?.release(); candidate?.release()
                if (error is SecurityException) throw error
            }
        }
        val active = requireNotNull(recorder)
        val agc = gainControl?.diagnostics ?: RecorderGainDiagnostics()
        val sourceName = if (source == MediaRecorder.AudioSource.MIC) "MIC" else "DEFAULT"
        Log.i("HearfolioRecording", "source=$sourceName; sampleRate=$rate; agcAvailable=${agc.available}; agcAttached=${agc.attached}; agcEnabled=${agc.enabled}; agcStatus=${agc.status}")
        capturing = true
        worker = Thread({ capture(active, file, rate) }, "HearfolioAudioCapture").also { it.start() }
    }
    private fun createGainControl(): RecorderGainControl = RecorderGainControl(
        isAvailable = { AutomaticGainControl.isAvailable() },
        create = { session ->
            AutomaticGainControl.create(session)?.let { effect ->
                object : RecorderGainEffect {
                    override fun isEnabled(): Boolean = effect.enabled
                    override fun enable(): Boolean = effect.setEnabled(true) == AudioEffect.SUCCESS
                    override fun release() { effect.release() }
                }
            }
        }
    )
    private fun releaseGainControl() {
        gainControl?.release()
        gainControl = null
    }


    private fun capture(active: AudioRecord, file: File, sampleRate: Int) {
        try {
            RandomAccessFile(file, "rw").use { wav ->
                wav.write(ByteArray(44))
                val samples = ShortArray(2048)
                val bytes = ByteArray(samples.size * 2)
                var count = 0L
                while (capturing) {
                    val read = active.read(samples, 0, samples.size, AudioRecord.READ_BLOCKING)
                    if (read < 0) { if (capturing) throw IllegalStateException("Capture failed"); break }
                    if (read == 0) continue
                    if (paused) { level = 0f; continue }
                    require(count + read * 2L <= 0xffffffffL - 36)
                    var energy = 0.0
                    for (index in 0 until read) {
                        val value = samples[index].toInt()
                        bytes[index * 2] = value.toByte()
                        bytes[index * 2 + 1] = (value shr 8).toByte()
                        val normalized = value / 32768.0
                        energy += normalized * normalized
                    }
                    val rms = sqrt(energy / read)
                    level = if (rms <= .001) 0f else ((20 * log10(rms) + 60) / 60).coerceIn(0.0, 1.0).toFloat()
                    wav.write(bytes, 0, read * 2)
                    count += read * 2L
                }
                wav.seek(0)
                fun little(value: Long, size: Int) { repeat(size) { wav.write((value shr (it * 8)).toInt() and 255) } }
                wav.writeBytes("RIFF"); little(count + 36, 4); wav.writeBytes("WAVEfmt ")
                little(16, 4); little(1, 2); little(1, 2); little(sampleRate.toLong(), 4)
                little(sampleRate * 2L, 4); little(2, 2); little(16, 2)
                wav.writeBytes("data"); little(count, 4); wav.fd.sync()
            }
        } catch (_: Exception) { failed = true; capturing = false; level = 0f; onFailure() }
    }
    fun pause() { paused = true; level = 0f }
    fun resume() { paused = false }
    fun stop(): Boolean {
        capturing = false
        val active = recorder; recorder = null
        try { active?.stop() } catch (_: Exception) { failed = true }
        worker?.join(250)
        releaseGainControl(); active?.release(); worker?.join(2000)
        if (worker?.isAlive == true) failed = true else worker = null
        level = 0f
        return !failed
    }
    fun release() { stop() }
}
