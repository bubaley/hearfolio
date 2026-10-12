package com.bubaley.hearfolio.recording

import java.io.File
import java.io.RandomAccessFile
import org.junit.Assert.*
import org.junit.Test

class BackgroundRecoveryTest {
    private class Lock(private val fail: Boolean = false) : OperationLock {
        var acquired = 0
        var released = 0
        var timeout = 0L
        override fun acquire(timeoutMillis: Long) { if(fail) throw IllegalStateException("unavailable"); acquired++; timeout = timeoutMillis }
        override fun release() { released++ }
    }
    @Test fun localWorkDoesNotAcquireNetworkAndTerminalCleanupIsIdempotent() {
        val cpu=Lock();val resources=OperationResources(cpu,null)
        resources.acquire(5000);resources.close();resources.close()
        assertEquals(5000,cpu.timeout);assertEquals(1,cpu.acquired);assertEquals(1,cpu.released)
    }
    @Test fun failedNetworkAcquisitionReleasesCpuAndSuccessfulWorkReleasesBoth() {
        val cpu=Lock();val network=Lock(true);val resources=OperationResources(cpu,network)
        try { resources.acquire(5000);fail("must fail") } catch(_:IllegalStateException) {}
        resources.close();assertEquals(1,cpu.released);assertEquals(0,network.released)
        val secondCpu=Lock();val secondNetwork=Lock();val second=OperationResources(secondCpu,secondNetwork)
        second.acquire(5000);second.close();second.close()
        assertEquals(1,secondCpu.released);assertEquals(1,secondNetwork.released)
    }
    @Test fun killedCaptureRepairsLengthFromActualPcmWithoutRepeatingAudio() {
        val file=File.createTempFile("hearfolio-recording-", ".wav.partial")
        try {
            RandomAccessFile(file,"rw").use { wav ->
                WavRecovery.header(wav,0,16000)
                wav.write(ByteArray(32000))
                wav.write(1) // Incomplete sample is trimmed after process interruption.
            }
            assertEquals(1L,WavRecovery.recover(file))
            RandomAccessFile(file,"r").use { wav ->
                assertEquals(32044L,wav.length());wav.seek(40)
                assertEquals(0,wav.read());assertEquals(125,wav.read())
            }
            assertEquals(1L,WavRecovery.recover(file))
        } finally { file.delete() }
    }
    @Test fun invalidOrEmptyHeaderIsPreservedAndNotPresentedAsRecoveredAudio() {
        val file=File.createTempFile("hearfolio-recording-", ".wav.partial")
        try {
            file.writeBytes(ByteArray(100));assertNull(WavRecovery.recover(file));assertEquals(100L,file.length())
            RandomAccessFile(file,"rw").use { wav -> wav.setLength(0);WavRecovery.header(wav,0,16000) }
            assertNull(WavRecovery.recover(file))
        } finally { file.delete() }
    }
}
