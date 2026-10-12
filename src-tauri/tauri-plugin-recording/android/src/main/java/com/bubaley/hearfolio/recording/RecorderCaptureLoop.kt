package com.bubaley.hearfolio.recording

import java.io.File
import java.io.RandomAccessFile
import java.util.ArrayDeque
import kotlin.math.log10
import kotlin.math.sqrt

internal interface RecorderInput {
    // Nonblocking: only the capture worker owns reads, replacement and close.
    fun read(samples: ShortArray): Int
    fun close()
}

internal object RecorderControlPolicy {
    fun accepts(path: String, requestedPath: String, state: String) =
        path.isNotEmpty() && path == requestedPath && (state == "recording" || state == "paused")
    fun canAcknowledge(state: String) = state != "recording" && state != "paused" && state != "stopping"
    fun canResetMetadata(hasCapture: Boolean) = !hasCapture
    fun shouldRecoverOrphan(state: String, hasCapture: Boolean) = !hasCapture &&
        state in listOf("starting", "recording", "paused", "stopping")
}

internal data class RecorderEnvelope(val levels: List<Float>, val sequence: Long, val elapsedMillis: Long)

internal class RecorderEnvelopeHistory(private val rate: Int, private val capacity: Int = 480) {
    private val levels = ArrayDeque<Float>()
    private var sequence = 0L
    private var frames = 0L
    private var bucketFrames = 0
    private var energy = 0.0
    @Volatile var level = 0f
        private set
    @Volatile var elapsedMillis = 0L
        private set
    @Synchronized fun append(samples: ShortArray, count: Int) {
        for (index in 0 until count) {
            val normalized = samples[index] / 32768.0
            energy += normalized * normalized
            bucketFrames++
            frames++
            // Rational boundaries avoid drift at 44100 Hz (125ms is 5512.5 frames).
            if (frames * 8 >= (sequence + 1) * rate) {
                val rms = sqrt(energy / bucketFrames)
                level = if (rms <= .001) 0f else ((20 * log10(rms) + 60) / 60).coerceIn(0.0, 1.0).toFloat()
                levels.addLast(level)
                if (levels.size > capacity) levels.removeFirst()
                sequence++; bucketFrames = 0; energy = 0.0
            }
        }
        elapsedMillis = frames * 1000 / rate
    }
    @Synchronized fun snapshot() = RecorderEnvelope(levels.toList(), sequence, elapsedMillis)
}

// Hardware reconfiguration must replace AudioRecord, not finish the WAV. The
// format is fixed for the whole file; recreation always uses that exact format.
internal class RecorderCaptureLoop(
    private val file: File,
    private val rate: Int,
    initial: RecorderInput,
    private val reopen: () -> RecorderInput,
    private val onFailure: (Exception) -> Unit,
    private val onRecovery: (Int) -> Unit = {},
    private val now: () -> Long = { System.nanoTime() / 1_000_000 },
    private val idle: () -> Unit = { Thread.sleep(10) },
    private val retryWait: () -> Unit = { Thread.sleep(100) },
    private val sync: (RandomAccessFile) -> Unit = { it.fd.sync() }
) {
    val envelope = RecorderEnvelopeHistory(rate)
    private val gate = Any()
    private var input: RecorderInput? = initial
    @Volatile private var running = true
    @Volatile private var paused = false
    private var phaseRevision = 0L
    @Volatile var failed = false
        private set
    @Volatile var closed = false
        private set
    fun pause(value: Boolean) = synchronized(gate) { if(paused!=value) { paused=value;phaseRevision++ } }
    fun stop() = synchronized(gate) { running = false }
    fun run() {
        var failure: Exception? = null
        try {
            RandomAccessFile(file, "rw").use { wav ->
                WavRecovery.header(wav, 0, rate); sync(wav)
                val samples = ShortArray(2048)
                val bytes = ByteArray(samples.size * 2)
                val recoveries = ArrayDeque<Long>()
                var count = 0L
                var checkpoint = 0L
                var lastData = now()
                fun recover(code: Int) {
                    val time = now()
                    while (recoveries.isNotEmpty() && time - recoveries.peekFirst() >= 30_000) recoveries.removeFirst()
                    check(recoveries.size < 3) { "Audio input recovery exhausted: $code" }
                    recoveries.addLast(time)
                    val old=input;input=null;old?.close()
                    var failure:Exception?=null
                    repeat(3) { attempt ->
                        if(!running) return
                        try {
                            input = reopen();lastData = now();onRecovery(code);return
                        } catch(error:Exception) {
                            if(error is SecurityException) throw error
                            failure=error
                            if(!running) return
                            if(attempt<2) retryWait()
                        }
                    }
                    throw requireNotNull(failure)
                }
                try {
                    while (running) {
                        val revision=synchronized(gate) {phaseRevision}
                        val read = requireNotNull(input).read(samples)
                        if (!running) break
                        if (read < 0) {
                            // Android: DEAD_OBJECT=-6, INVALID_OPERATION=-3, ERROR=-1.
                            // BAD_VALUE=-2 is a programming/configuration error.
                            check(read == -6 || read == -3 || read == -1) { "Audio input read failed: $read" }
                            recover(read); continue
                        }
                        if (read == 0) {
                            if (now() - lastData >= 5000) recover(0) else idle()
                            continue
                        }
                        require(read <= samples.size)
                        lastData = now()
                        // Accept at one short control boundary. Once accepted,
                        // this pre-pause buffer belongs to the WAV; disk/fsync
                        // latency must never prevent a stop/pause request.
                        val accepted=synchronized(gate) {running && !paused && phaseRevision==revision}
                        if(accepted) {
                            require(count + read * 2L <= 0xffffffffL - 36)
                            for(index in 0 until read) {
                                val value=samples[index].toInt()
                                bytes[index*2]=value.toByte();bytes[index*2+1]=(value shr 8).toByte()
                            }
                            wav.write(bytes,0,read*2)
                            count+=read*2L
                            envelope.append(samples,read)
                            if(count-checkpoint>=rate*4L) {WavRecovery.header(wav,count,rate);sync(wav);checkpoint=count}
                        }
                    }
                } finally {
                    WavRecovery.header(wav, count, rate); sync(wav)
                }
            }
        } catch (error: Exception) {
            failed = true; failure = error
        } finally {
            running = false
            try { input?.close() } catch(error:Exception) { failed=true;if(failure==null)failure=error }
            input = null
            closed = true
        }
        failure?.let(onFailure)
    }
}
