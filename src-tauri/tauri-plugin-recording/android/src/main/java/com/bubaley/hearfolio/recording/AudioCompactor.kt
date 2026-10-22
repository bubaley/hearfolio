package com.bubaley.hearfolio.recording

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.MediaMetadataRetriever
import android.media.MediaMuxer
import android.os.SystemClock
import java.io.File
import java.io.RandomAccessFile

// Canonical 44-byte PCM16 WAV written by RecorderCaptureLoop (and repaired by
// WavRecovery). Anything else is rejected so the caller keeps the original.
internal data class PcmWav(val sampleRate: Int, val channels: Int, val dataBytes: Long) {
    val frames: Long get() = dataBytes / (2L * channels)
    val durationMillis: Long get() = frames * 1000 / sampleRate

    companion object {
        fun read(file: File): PcmWav? = try {
            RandomAccessFile(file, "r").use { wav ->
                if (wav.length() <= 44) return null
                val bytes = ByteArray(44); wav.readFully(bytes)
                fun tag(offset: Int, value: String) = String(bytes, offset, value.length, Charsets.US_ASCII) == value
                fun little(offset: Int, size: Int): Long { var value = 0L; repeat(size) { value = value or ((bytes[offset + it].toLong() and 255) shl (8 * it)) }; return value }
                if (!tag(0, "RIFF") || !tag(8, "WAVEfmt ") || !tag(36, "data") || little(16, 4) != 16L || little(20, 2) != 1L || little(34, 2) != 16L) return null
                val channels = little(22, 2).toInt()
                val rate = little(24, 4).toInt()
                if (channels !in 1..2 || rate !in 8000..48000) return null
                val block = 2L * channels
                // Compact only a finalized file: a header that disagrees with the
                // actual PCM length could drop audio that the WAV still holds.
                val data = (wav.length() - 44) / block * block
                if (data <= 0 || little(40, 4) != data) null else PcmWav(rate, channels, data)
            }
        } catch (_: Exception) { null }
    }
}

// Transcodes a finished recording WAV to AAC-LC in an MP4 (.m4a) container.
// The output is written to a caller-owned path that must not exist; on any
// failure it is deleted and the WAV is left untouched.
internal object AudioCompactor {
    private const val TIMEOUT_US = 10_000L
    private const val STALL_MS = 15_000L

    fun encodeAac(input: File, output: File, bitrate: Int): Long {
        require(!output.exists())
        val wav = requireNotNull(PcmWav.read(input)) { "Unsupported WAV" }
        try {
            transcode(input, output, wav, bitrate.coerceIn(24_000, 128_000))
            // Verify the container independently of the encoder that wrote it.
            val duration = MediaMetadataRetriever().let { retriever ->
                try {
                    retriever.setDataSource(output.path)
                    retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()
                } finally { retriever.release() }
            } ?: throw IllegalStateException("Unreadable AAC output")
            check(output.length() > 0 && kotlin.math.abs(duration - wav.durationMillis) <= 250 + wav.durationMillis / 200) {
                "AAC duration $duration does not match ${wav.durationMillis}"
            }
            RandomAccessFile(output, "rw").use { it.fd.sync() }
            return duration
        } catch (error: Exception) {
            output.delete()
            throw error
        }
    }

    private fun transcode(input: File, output: File, wav: PcmWav, bitrate: Int) {
        val format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, wav.sampleRate, wav.channels).apply {
            setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
            setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 16384)
        }
        val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
        var muxer: MediaMuxer? = null
        var muxing = false
        try {
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            codec.start()
            val activeMuxer = MediaMuxer(output.path, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)
            muxer = activeMuxer
            RandomAccessFile(input, "r").use { source ->
                source.seek(44)
                val block = 2 * wav.channels
                val chunk = ByteArray(8192 / block * block)
                val info = MediaCodec.BufferInfo()
                var consumed = 0L
                var track = -1
                var inputDone = false
                var outputDone = false
                var progress = SystemClock.elapsedRealtime()
                while (!outputDone) {
                    check(SystemClock.elapsedRealtime() - progress < STALL_MS) { "AAC encoder stalled" }
                    if (!inputDone) {
                        val index = codec.dequeueInputBuffer(TIMEOUT_US)
                        if (index >= 0) {
                            val buffer = requireNotNull(codec.getInputBuffer(index))
                            buffer.clear()
                            val size = minOf(buffer.remaining().toLong() / block * block, chunk.size.toLong(), wav.dataBytes - consumed).toInt()
                            val time = consumed / block * 1_000_000L / wav.sampleRate
                            if (size <= 0) {
                                codec.queueInputBuffer(index, 0, 0, time, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                                inputDone = true
                            } else {
                                source.readFully(chunk, 0, size)
                                buffer.put(chunk, 0, size)
                                codec.queueInputBuffer(index, 0, size, time, 0)
                                consumed += size
                            }
                            progress = SystemClock.elapsedRealtime()
                        }
                    }
                    val index = codec.dequeueOutputBuffer(info, TIMEOUT_US)
                    if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                        check(!muxing)
                        track = activeMuxer.addTrack(codec.outputFormat)
                        activeMuxer.start(); muxing = true
                    } else if (index >= 0) {
                        val buffer = requireNotNull(codec.getOutputBuffer(index))
                        if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) info.size = 0
                        if (info.size > 0) {
                            check(muxing)
                            buffer.position(info.offset); buffer.limit(info.offset + info.size)
                            activeMuxer.writeSampleData(track, buffer, info)
                        }
                        codec.releaseOutputBuffer(index, false)
                        if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) outputDone = true
                        progress = SystemClock.elapsedRealtime()
                    }
                }
                check(consumed == wav.dataBytes)
            }
            check(muxing)
            activeMuxer.stop(); muxing = false
        } finally {
            try { codec.stop() } catch (_: Exception) { }
            try { codec.release() } catch (_: Exception) { }
            if (muxing) try { muxer?.stop() } catch (_: Exception) { }
            try { muxer?.release() } catch (_: Exception) { }
        }
    }
}
