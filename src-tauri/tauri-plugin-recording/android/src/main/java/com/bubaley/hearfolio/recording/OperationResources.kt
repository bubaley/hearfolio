package com.bubaley.hearfolio.recording

internal interface OperationLock {
    fun acquire(timeoutMillis: Long)
    fun release()
}

// Single owner for CPU/network resources. Failed startup and every terminal
// service path share the same idempotent cleanup, including partial acquisition.
internal class OperationResources(private val cpu: OperationLock, private val network: OperationLock?) {
    private var cpuHeld = false
    private var networkHeld = false
    private var closed = false
    fun acquire(timeoutMillis: Long) {
        check(!closed && !cpuHeld)
        try {
            cpu.acquire(timeoutMillis); cpuHeld = true
            network?.let { it.acquire(timeoutMillis); networkHeld = true }
        } catch(error: Exception) { close(); throw error }
    }
    fun close() {
        if(closed) return
        closed = true
        try { if(networkHeld) network?.release() } finally { if(cpuHeld) cpu.release() }
        networkHeld = false; cpuHeld = false
    }
}
