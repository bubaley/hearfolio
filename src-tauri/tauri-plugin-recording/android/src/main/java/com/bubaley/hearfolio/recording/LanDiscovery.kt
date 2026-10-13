package com.bubaley.hearfolio.recording

import android.content.Context
import android.net.wifi.WifiManager
import java.io.File

// Rust DNS-SD watches interface changes; this lock lets its multicast reach Wi-Fi.
internal object LanDiscovery {
    private val owners = mutableSetOf<String>()
    private var lock: WifiManager.MulticastLock? = null
    @Synchronized fun acquire(context: Context, owner: String) {
        if (!owners.add(owner)) return
        if (lock != null) return
        try {
            val wifi = context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager ?: return
            lock = wifi.createMulticastLock("Hearfolio:discovery").apply {setReferenceCounted(false);acquire()}
        } catch (_: Exception) { owners.remove(owner);lock = null }
    }
    @Synchronized fun release(owner: String) {
        owners.remove(owner)
        if (owners.isEmpty()) { if (lock?.isHeld == true) lock?.release();lock = null }
    }
    fun waiting(state: File?): Boolean = NativeJobJournal.read(state?.parentFile?.resolve("lan-network-state.json"))?.optBoolean("waiting") == true
}
