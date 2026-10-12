package com.bubaley.hearfolio.recording

import java.io.File
import java.io.RandomAccessFile
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test

class RecorderCaptureLoopTest {
    private class Input(var readNext: (ShortArray) -> Int) : RecorderInput {
        var closed = 0
        override fun read(samples: ShortArray) = readNext(samples)
        override fun close() { closed++ }
    }
    private fun pcm(samples: ShortArray, value: Int): Int { samples.fill(value.toShort(),0,1000);return 1000 }
    private fun header(file: File, expectedFrames: Long) {
        RandomAccessFile(file,"r").use { wav ->
            assertEquals(44+expectedFrames*2,wav.length())
            wav.seek(24);assertEquals(64,wav.read());assertEquals(31,wav.read()) // 8000Hz
            wav.seek(40);val size=ByteArray(4);wav.readFully(size)
            val length=size.indices.fold(0L){value,index -> value or ((size[index].toLong() and 255) shl (index*8))}
            assertEquals(expectedFrames*2,length)
        }
    }
    @Test fun deadObjectRecreatesInputAndAppendsToSameWavWithOriginalFormat() {
        val file=File.createTempFile("hearfolio-capture-", ".wav")
        try {
            lateinit var loop:RecorderCaptureLoop
            var firstRead=0;var secondRead=0;var reopened=0
            val original=Input { samples -> if(firstRead++==0)pcm(samples,1000) else -6 }
            val replacement=Input { samples -> if(secondRead++==0)pcm(samples,9000) else {loop.stop();-3} }
            val failures=mutableListOf<Exception>();val recoveryCodes=mutableListOf<Int>()
            loop=RecorderCaptureLoop(file,8000,original,{reopened++;replacement},{failures.add(it)},{recoveryCodes.add(it)})
            loop.run()
            assertTrue(failures.isEmpty());assertFalse(loop.failed)
            assertEquals(1,reopened);assertEquals(listOf(-6),recoveryCodes)
            assertEquals(1,original.closed);assertEquals(1,replacement.closed)
            header(file,2000)
            RandomAccessFile(file,"r").use { wav ->
                wav.seek(44);assertEquals(232,wav.read());assertEquals(3,wav.read())
                wav.seek(2044);assertEquals(40,wav.read());assertEquals(35,wav.read())
            }
            val history=loop.envelope.snapshot();assertEquals(250L,history.elapsedMillis);assertEquals(2L,history.sequence)
            assertTrue(history.levels[0]<history.levels[1])
        } finally {file.delete()}
    }
    @Test fun pauseDrainsWithoutSavingAndResumeKeepsPcmDurationAndHistoryAcrossUiAbsence() {
        val file=File.createTempFile("hearfolio-capture-", ".wav")
        try {
            lateinit var loop:RecorderCaptureLoop
            var step=0;var pausedHistory:RecorderEnvelope?=null
            val input=Input { samples -> when(step++) {
                0 -> pcm(samples,1000)
                1 -> {loop.pause(true);pausedHistory=loop.envelope.snapshot();pcm(samples,30000)}
                2 -> {assertEquals(pausedHistory,loop.envelope.snapshot());pcm(samples,30000)}
                3 -> {assertEquals(pausedHistory,loop.envelope.snapshot());loop.pause(false);pcm(samples,30000)}
                4 -> pcm(samples,8000)
                else -> {loop.stop();-3}
            } }
            val failures=mutableListOf<Exception>()
            loop=RecorderCaptureLoop(file,8000,input,{throw IllegalStateException("must retain input")},{failures.add(it)})
            loop.run();header(file,2000)
            assertEquals(125L,pausedHistory!!.elapsedMillis);assertEquals(250L,loop.envelope.elapsedMillis)
            assertEquals(2L,loop.envelope.snapshot().sequence);assertEquals(1,input.closed)
            assertTrue(failures.isEmpty());assertFalse(loop.failed)
        } finally {file.delete()}
    }
    @Test fun stalledNonblockingReadRecoversWithoutWaitingForActivityOrClosingWav() {
        val file=File.createTempFile("hearfolio-capture-", ".wav")
        try {
            lateinit var loop:RecorderCaptureLoop
            var time=0L;var step=0;var reopened=0
            val initial=Input {0}
            val replacement=Input { samples -> if(step++==0)pcm(samples,5000) else {loop.stop();0} }
            loop=RecorderCaptureLoop(file,8000,initial,{reopened++;replacement},{throw AssertionError(it)},now={time},idle={time+=1000})
            loop.run();header(file,1000)
            assertEquals(1,reopened);assertEquals(5000L,time)
            assertEquals(1,initial.closed);assertEquals(1,replacement.closed)
        } finally {file.delete()}
    }
    @Test fun recoveryIsBoundedAndRetainsValidWrittenPcmWhenHardwareStaysUnavailable() {
        val file=File.createTempFile("hearfolio-capture-", ".wav")
        try {
            var step=0;val original=Input {samples -> if(step++==0)pcm(samples,5000) else -6}
            val inputs=mutableListOf(original);val failures=mutableListOf<Exception>()
            val loop=RecorderCaptureLoop(file,8000,original,{Input {-6}.also {inputs.add(it)}},{failures.add(it)},now={0})
            loop.run();header(file,1000)
            assertTrue(loop.failed);assertEquals(1,failures.size);assertTrue(failures[0].message!!.contains("exhausted"))
            assertEquals(4,inputs.size);assertTrue(inputs.all {it.closed==1})
        } finally {file.delete()}
    }
    @Test fun transientReopenFailureRetriesAndStopDuringRetryDoesNotReopenAgain() {
        val file=File.createTempFile("hearfolio-capture-", ".wav")
        try {
            lateinit var loop:RecorderCaptureLoop
            var attempt=0;var step=0;var waited=0
            val original=Input {-6}
            val replacement=Input {samples -> if(step++==0)pcm(samples,5000) else {loop.stop();0}}
            loop=RecorderCaptureLoop(file,8000,original,{
                attempt++;if(attempt==1)throw IllegalStateException("route changing");replacement
            },{throw AssertionError(it)},retryWait={waited++})
            loop.run();header(file,1000)
            assertEquals(2,attempt);assertEquals(1,waited);assertEquals(1,original.closed);assertEquals(1,replacement.closed)
            var stoppedAttempts=0;val failures=mutableListOf<Exception>();val secondInput=Input {-6}
            lateinit var second:RecorderCaptureLoop
            second=RecorderCaptureLoop(file,8000,secondInput,{stoppedAttempts++;throw IllegalStateException("route changing")},{failures.add(it)},retryWait={second.stop()})
            second.run();assertEquals(1,stoppedAttempts);assertEquals(1,secondInput.closed);assertFalse(second.failed);assertTrue(failures.isEmpty())
        } finally {file.delete()}
    }
    @Test fun closeFailureStillReportsFailureAndReopenFailureDoesNotCloseOldInputTwice() {
        val file=File.createTempFile("hearfolio-capture-", ".wav")
        try {
            lateinit var loop:RecorderCaptureLoop
            val failures=mutableListOf<Exception>()
            val input=object:RecorderInput {
                override fun read(samples:ShortArray):Int {loop.stop();return -3}
                override fun close() {throw IllegalStateException("close failed")}
            }
            loop=RecorderCaptureLoop(file,8000,input,{throw AssertionError("must not reopen after stop")},{failures.add(it)})
            loop.run();assertTrue(loop.failed);assertEquals("close failed",failures.single().message)
            val old=Input {-6};failures.clear()
            val second=RecorderCaptureLoop(file,8000,old,{throw IllegalStateException("reopen failed")},{failures.add(it)})
            second.run();assertEquals(1,old.closed);assertEquals("reopen failed",failures.single().message)
        } finally {file.delete()}
    }
    @Test fun historyUsesMeasuredRmsHasBoundedTailAndRationalSampleBoundaries() {
        val history=RecorderEnvelopeHistory(44100,capacity=3)
        val samples=ShortArray(44100) {if(it<22050) 500 else 10000}
        history.append(samples,samples.size)
        val snapshot=history.snapshot()
        assertEquals(8L,snapshot.sequence);assertEquals(1000L,snapshot.elapsedMillis);assertEquals(3,snapshot.levels.size)
        assertTrue(snapshot.levels.all {it in 0f..1f})
        val silent=RecorderEnvelopeHistory(8000);silent.append(ShortArray(1000),1000)
        assertEquals(listOf(0f),silent.snapshot().levels)
    }
    @Test fun pendingStopDoesNotCloseOrPublishUntilWorkerActuallyLeavesItsRead() {
        val file=File.createTempFile("hearfolio-capture-", ".wav")
        val entered=CountDownLatch(1);val resume=CountDownLatch(1)
        var thread:Thread?=null
        try {
            var step=0;val failures=mutableListOf<Exception>()
            val input=Input {samples -> if(step++==0)pcm(samples,5000) else {entered.countDown();resume.await(2,TimeUnit.SECONDS);0}}
            val loop=RecorderCaptureLoop(file,8000,input,{throw AssertionError("not needed")},{failures.add(it)})
            val worker=Thread {loop.run()};thread=worker;worker.start()
            assertTrue(entered.await(1,TimeUnit.SECONDS));loop.stop()
            assertFalse(loop.closed);assertEquals(0,input.closed)
            // Initial header is still a checkpoint, never a terminal result.
            RandomAccessFile(file,"r").use {wav -> wav.seek(40);assertEquals(0,wav.read())}
            resume.countDown();worker.join(2000)
            assertFalse(worker.isAlive);assertTrue(loop.closed);assertEquals(1,input.closed)
            assertTrue(failures.isEmpty());header(file,1000)
        } finally {resume.countDown();thread?.join(2000);file.delete()}
    }
    @Test fun staleSessionControlsAndAcknowledgementCannotChangeActiveCapture() {
        assertFalse(RecorderControlPolicy.accepts("/new.wav","/old.wav","paused"))
        assertFalse(RecorderControlPolicy.accepts("/new.wav","/new.wav","finished"))
        assertTrue(RecorderControlPolicy.accepts("/new.wav","/new.wav","paused"))
        assertTrue(RecorderControlPolicy.accepts("/new.wav","/new.wav","recording"))
        assertFalse(RecorderControlPolicy.canAcknowledge("recording"));assertFalse(RecorderControlPolicy.canAcknowledge("paused"))
        assertFalse(RecorderControlPolicy.canAcknowledge("stopping"))
        assertTrue(RecorderControlPolicy.canAcknowledge("finished"))
        assertTrue(RecorderControlPolicy.canResetMetadata(false));assertFalse(RecorderControlPolicy.canResetMetadata(true))
    }
}
