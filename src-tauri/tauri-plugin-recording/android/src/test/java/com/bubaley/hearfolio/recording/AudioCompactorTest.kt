package com.bubaley.hearfolio.recording

import java.io.File
import java.io.RandomAccessFile
import org.junit.Assert.*
import org.junit.Test

class AudioCompactorTest {
    @Test fun compactorReadsOnlyFinalizedCanonicalPcm() {
        val file = File.createTempFile("hearfolio-recording-", ".wav")
        try {
            RandomAccessFile(file, "rw").use { wav ->
                WavRecovery.header(wav, 0, 16000)
                wav.write(ByteArray(32000))
            }
            // A stale header (killed before a checkpoint) is never compacted:
            // encoding only the declared length would drop captured audio.
            assertNull(PcmWav.read(file))
            assertEquals(1L, WavRecovery.recover(file))
            val wav = requireNotNull(PcmWav.read(file))
            assertEquals(16000, wav.sampleRate)
            assertEquals(1, wav.channels)
            assertEquals(32000L, wav.dataBytes)
            assertEquals(1000L, wav.durationMillis)
            RandomAccessFile(file, "rw").use { it.setLength(44) }
            assertNull(PcmWav.read(file))
            file.writeText("not a wav file at all, but long enough to have a header")
            assertNull(PcmWav.read(file))
        } finally { file.delete() }
    }
}
