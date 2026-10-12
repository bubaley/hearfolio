package com.bubaley.hearfolio.recording

import java.util.concurrent.Executors
import java.util.concurrent.ExecutorService
import java.util.concurrent.FutureTask

// Application scope: Activity recreation never replaces the command queue.
internal class RecorderLifecycleQueue(private val executor: ExecutorService = Executors.newSingleThreadExecutor { task -> Thread(task, "Hearfolio-recording-control") }) {
    fun submit(action: () -> Unit) { executor.execute(action) }
    // Called only by the control worker for the first durable identity. Periodic
    // updates use submit; a new capture cannot overtake a previous disk write.
    fun commit(action: () -> Unit) {
        val task=FutureTask<Unit> {action();Unit}
        executor.execute(task);task.get()
    }
    fun close() { executor.shutdownNow() }
}

// A queued service intent is not ownership of the microphone. Cancellation
// revokes its ticket even after dispatch, until the capture is actually started.
internal class RecorderStartRegistry {
    class Ticket(val path: String, val quick: Boolean, @Volatile var revision: Long, val callback: (String?) -> Unit) {
        @Volatile var claimed = false
        @Volatile var cancelled = false
    }
    @Volatile private var current: Ticket? = null
    val pendingTicket: Ticket? get() = current
    val pendingPath: String get() = current?.path ?: ""
    val pendingQuick: Boolean get() = current?.quick ?: false
    val pendingRevision: Long get() = current?.revision ?: 0L
    @Synchronized fun reserve(path: String, quick: Boolean = false, revision: Long = 0L, callback: (String?) -> Unit): Ticket? {
        if(current != null) return null
        return Ticket(path, quick, revision, callback).also { current=it }
    }
    @Synchronized fun claim(path: String): Ticket? = current?.takeIf { it.path==path && !it.cancelled }?.also { it.claimed=true }
    @Synchronized fun complete(ticket: Ticket, error: String?) {
        if(current !== ticket) return
        current=null
        ticket.callback(error)
    }
    @Synchronized fun cancel(path: String, onlyUnclaimed: Boolean = false): Boolean {
        val ticket=current ?: return false
        if(ticket.path!=path || (onlyUnclaimed && ticket.claimed)) return false
        ticket.cancelled=true;current=null
        ticket.callback("errors.recordingUnavailable")
        return true
    }
}
