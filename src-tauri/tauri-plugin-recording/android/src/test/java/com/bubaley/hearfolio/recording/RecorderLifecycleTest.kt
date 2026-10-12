package com.bubaley.hearfolio.recording

import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test

class RecorderLifecycleTest {
    @Test fun blockedLifecycleWorkDoesNotBlockCallerAndCommandsKeepOrder() {
        val queue=RecorderLifecycleQueue()
        val entered=CountDownLatch(1);val release=CountDownLatch(1);val finished=CountDownLatch(1)
        val events=mutableListOf<String>();val caller=Thread.currentThread()
        try {
            queue.submit { assertNotSame(caller,Thread.currentThread());events.add("stop requested");entered.countDown();release.await(3,TimeUnit.SECONDS);events.add("worker closed") }
            assertTrue(entered.await(2,TimeUnit.SECONDS))
            queue.submit {events.add("acknowledged");events.add("new start");finished.countDown()}
            assertFalse(finished.await(50,TimeUnit.MILLISECONDS))
            release.countDown();assertTrue(finished.await(2,TimeUnit.SECONDS))
            assertEquals(listOf("stop requested","worker closed","acknowledged","new start"),events)
        } finally {release.countDown();queue.close()}
    }
    @Test fun initialIdentityMustCommitBeforeMicrophoneOwnershipOrStartAcknowledgement() {
        val controls=RecorderLifecycleQueue();val writes=RecorderLifecycleQueue()
        val blocked=CountDownLatch(1);val release=CountDownLatch(1);val ready=CountDownLatch(1)
        val opened=AtomicInteger();val identities=mutableListOf<String>()
        try {
            writes.submit {blocked.countDown();release.await(3,TimeUnit.SECONDS);identities.add("previous")}
            assertTrue(blocked.await(2,TimeUnit.SECONDS))
            controls.submit {writes.commit {identities.add("starting new.wav")};opened.incrementAndGet();ready.countDown()}
            assertFalse(ready.await(50,TimeUnit.MILLISECONDS));assertEquals(0,opened.get())
            release.countDown();assertTrue(ready.await(2,TimeUnit.SECONDS))
            assertEquals(listOf("previous","starting new.wav"),identities);assertEquals(1,opened.get())
        } finally {release.countDown();controls.close();writes.close()}
    }
    @Test fun durableStartingWithPcmRecoversAfterProcessDeathWithoutRecordingCheckpoint() {
        val file=File.createTempFile("hearfolio-starting-", ".wav")
        try {
            // The first checkpoint says starting. The worker has appended PCM,
            // but the process died before the async recording checkpoint/header.
            java.io.RandomAccessFile(file,"rw").use {wav ->
                WavRecovery.header(wav,0,8000);wav.write(ByteArray(16000))
            }
            assertTrue(RecorderControlPolicy.shouldRecoverOrphan("starting",false))
            assertEquals(1L,WavRecovery.recover(file))
            assertEquals(16044L,file.length())
            for(state in listOf("starting","recording","paused","stopping")) {
                assertTrue(RecorderControlPolicy.shouldRecoverOrphan(state,false))
                assertFalse("Never repair a live worker WAV",RecorderControlPolicy.shouldRecoverOrphan(state,true))
            }
            for(state in listOf("idle","finished","cancelled","error")) assertFalse(RecorderControlPolicy.shouldRecoverOrphan(state,false))
        } finally {file.delete()}
    }
    @Test fun rejectedOrCancelledForegroundStartPromotesBeforeStoppingWithoutOpeningCapture() {
        val events=mutableListOf<String>()
        val accepted=RecorderForegroundStart.accept(false,{events.add("startForeground")},{events.add("stopSelf")})
        if(accepted) events.add("open microphone")
        assertFalse(accepted);assertEquals(listOf("startForeground","stopSelf"),events)
        events.clear()
        assertTrue(RecorderForegroundStart.accept(true,{events.add("startForeground")},{events.add("stopSelf")}))
        assertEquals(listOf("startForeground"),events)
    }
    @Test fun aliasedAndroidDataPathUsesSameIdentityForReservationIntentAndControls() {
        val root=java.nio.file.Files.createTempDirectory("hearfolio-data-alias-")
        val actual=java.nio.file.Files.createDirectory(root.resolve("user-0"))
        val alias=java.nio.file.Files.createSymbolicLink(root.resolve("data-data"),actual)
        try {
            val raw=alias.resolve("hearfolio-recording-123.wav").toString()
            val intentPath=File(raw).canonicalPath
            assertNotEquals(raw,intentPath)
            // Reproduce the released wiring: raw reservation + canonical Intent.
            val oldRegistry=RecorderStartRegistry()
            oldRegistry.reserve(raw) {}
            assertNull(oldRegistry.claim(intentPath))
            assertEquals(raw,oldRegistry.pendingPath)
            // Production canonicalizer runs before reservation and is reused
            // for stop/pause/cancel, even before the WAV exists.
            val registry=RecorderStartRegistry();val identity=RecorderPathIdentity.canonical(raw)
            val ticket=registry.reserve(identity) {}!!
            assertSame(ticket,registry.claim(intentPath))
            assertEquals(ticket.path,RecorderPathIdentity.canonical(raw))
            assertTrue(RecorderControlPolicy.accepts(ticket.path,RecorderPathIdentity.canonical(raw),"recording"))
            assertTrue(registry.cancel(RecorderPathIdentity.canonical(raw)))
        } finally {
            java.nio.file.Files.deleteIfExists(alias);java.nio.file.Files.deleteIfExists(actual);java.nio.file.Files.deleteIfExists(root)
        }
    }
    @Test fun cancellationBetweenServiceDispatchAndCaptureOpeningRevokesStart() {
        val registry=RecorderStartRegistry();val calls=mutableListOf<String?>()
        val ticket=registry.reserve("old.wav") {calls.add(it)}!!
        assertSame(ticket,registry.claim("old.wav"))
        assertTrue(registry.cancel("old.wav"));assertTrue(ticket.cancelled)
        assertNull(registry.claim("old.wav"))
        registry.complete(ticket,null) // late completion from the queued START
        assertEquals(listOf("errors.recordingUnavailable"),calls)
        assertEquals("",registry.pendingPath)
    }
    @Test fun activityDestructionRevokesPermissionsButPreservesClaimedServiceOwnership() {
        val registry=RecorderStartRegistry();val calls=mutableListOf<String?>()
        val permission=registry.reserve("permission.wav") {calls.add(it)}!!
        assertEquals("permission.wav",registry.pendingPath)
        assertTrue(registry.cancel(permission.path,onlyUnclaimed=true));assertNull(registry.claim(permission.path))
        val service=registry.reserve("service.wav",quick=true) {calls.add(it)}!!
        registry.claim(service.path)
        assertFalse(registry.cancel(service.path,onlyUnclaimed=true))
        assertFalse(service.cancelled);assertEquals(service.path,registry.pendingPath);assertTrue(registry.pendingQuick)
        registry.complete(service,null);assertEquals("",registry.pendingPath)
        assertEquals(listOf("errors.recordingUnavailable",null),calls)
    }
    @Test fun lateOldCompletionAndCancelCannotClaimReplacementSession() {
        val registry=RecorderStartRegistry();val oldCalls=AtomicInteger();val newCalls=AtomicInteger()
        val old=registry.reserve("old.wav") {oldCalls.incrementAndGet()}!!
        registry.cancel(old.path)
        val fresh=registry.reserve("fresh.wav") {newCalls.incrementAndGet()}!!
        registry.complete(old,null)
        assertFalse(registry.cancel(old.path));assertNull(registry.claim(old.path))
        assertEquals(fresh.path,registry.pendingPath);assertEquals(1,oldCalls.get());assertEquals(0,newCalls.get())
        registry.claim(fresh.path);registry.complete(fresh,null);assertEquals(1,newCalls.get())
    }
    @Test fun capturePauseAndStopRemainResponsiveWhileCheckpointIsBlocked() {
        val file=File.createTempFile("hearfolio-lifecycle-",".wav")
        val entered=CountDownLatch(1);val release=CountDownLatch(1);val done=CountDownLatch(1)
        var syncCount=0;val failures=mutableListOf<Exception>()
        val input=object:RecorderInput {
            var closed=false
            override fun read(samples:ShortArray):Int {samples.fill(7000);return samples.size}
            override fun close() {closed=true}
        }
        val loop=RecorderCaptureLoop(file,8000,input,{throw IllegalStateException("unexpected reopen")},{failures.add(it)},sync={ wav ->
            if(++syncCount==2) {entered.countDown();release.await(3,TimeUnit.SECONDS)}
            wav.fd.sync()
        })
        val worker=Thread {try {loop.run()} finally {done.countDown()}}
        try {
            worker.start();assertTrue(entered.await(2,TimeUnit.SECONDS))
            val controls=CountDownLatch(1)
            Thread {loop.pause(true);loop.pause(false);loop.stop();controls.countDown()}.start()
            assertTrue("Control waits behind WAV fsync",controls.await(500,TimeUnit.MILLISECONDS))
            assertFalse(loop.closed);assertFalse(input.closed)
            release.countDown();assertTrue(done.await(2,TimeUnit.SECONDS))
            assertTrue(loop.closed);assertTrue(input.closed);assertTrue(failures.isEmpty())
            val recovered=WavRecovery.recover(file)
            assertNotNull(recovered);assertTrue(loop.envelope.elapsedMillis>=2000)
        } finally {loop.stop();release.countDown();worker.join(3000);file.delete()}
    }
}
