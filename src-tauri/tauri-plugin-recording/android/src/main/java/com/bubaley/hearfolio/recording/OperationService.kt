package com.bubaley.hearfolio.recording

import android.app.*
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.*
import androidx.core.app.NotificationCompat
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import org.json.JSONObject
import android.util.AtomicFile

// Rust owns the work, this service owns its Android lifetime. No Activity,
// WebView timers, transcript, token or audio data is retained here.
class OperationService : Service() {
    companion object {
        const val BEGIN = "hearfolio.operation.BEGIN"
        const val CANCEL = "hearfolio.operation.CANCEL"
        const val CHANNEL = "hearfolio-operations"
        const val NOTIFICATION = 4104
        // Rust runs the same watchdog (30 min without progress, 12 h total) and
        // reports first; these only bound a Rust side that stopped responding.
        const val STALL_MILLIS = 31 * 60 * 1000L
        const val MAX_MILLIS = 12 * 60 * 60 * 1000L + 60 * 1000L
        const val READY_MILLIS = 10 * 1000L
        @Volatile var instance: OperationService? = null
        @Volatile var job: String? = null
        @Volatile var cancelled = false
        @Volatile var error: String? = null
        // One readiness callback per operation id. A lost service start resolves
        // with an error after READY_MILLIS, so Rust is never left waiting.
        private val ready = ConcurrentHashMap<String, (String?) -> Unit>()
        private val readyTimeouts = Handler(Looper.getMainLooper())
        fun expectReady(id: String, callback: (String?) -> Unit) {
            ready[id] = callback
            readyTimeouts.postDelayed({ resolveReady(id, "errors.operationBackgroundUnavailable") }, READY_MILLIS)
        }
        fun resolveReady(id: String, error: String?) { ready.remove(id)?.invoke(error) }
        fun isCancelled(id: String) = job != id || cancelled || instance == null
    }
    private var resources: OperationResources? = null
    private val handler = Handler(Looper.getMainLooper())
    private var kind = "transcribe"
    private var startedAt = 0L
    private var lastProgress: String? = null
    private var snapshotFile: File? = null
    private var controlFile: File? = null
    private val poll = object : Runnable {
        override fun run() {
            try {
                val snapshot=JSONObject(requireNotNull(snapshotFile).readText())
                if(snapshot.optString("id") == job) {
                    val state=snapshot.optString("state")
                    if(state != "running") { finish(state);return }
                    if(snapshot.optString("error") == "errors.operationCancelled") { cancelled=true;error="errors.operationCancelled";persistControl() }
                    val progress=snapshot.optJSONObject("progress")
                    val signature=progress?.toString() ?: ""
                    if(signature != lastProgress) { lastProgress=signature;armStall() }
                    update(progress?.optString("stage") ?: "",progress?.optLong("current") ?: 0,progress?.optLong("total") ?: 0)
                }
            } catch(_: Exception) { /* Atomic replacement may be momentarily unavailable; the deadline stays bounded. */ }
            handler.postDelayed(this,1000)
        }
    }
    private fun privateStateFile(path: String?): File {
        val file=File(requireNotNull(path)).canonicalFile
        require(file.path.startsWith(dataDir.canonicalPath+File.separator) && file.name.endsWith(".json"))
        return file
    }
    private fun persistControl() {
        val file=controlFile ?: return
        val atomic=AtomicFile(file)
        val stream=atomic.startWrite()
        try { stream.write(JSONObject().put("id",job).put("cancelled",cancelled).put("error",error).toString().toByteArray());atomic.finishWrite(stream) }
        catch(error:Exception) { atomic.failWrite(stream);throw error }
    }
    private val deadline = Runnable { abort("errors.operationTimeout") }
    private val stall = Runnable { abort("errors.operationTimeout") }
    private fun armStall() { handler.removeCallbacks(stall); handler.postDelayed(stall,STALL_MILLIS) }
    override fun onCreate() {
        super.onCreate(); instance = this
        if (Build.VERSION.SDK_INT >= 26) getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.hearfolio_operation_channel), NotificationManager.IMPORTANCE_LOW))
    }
    private fun chunked(stage: String) = stage == "progress.sendCloudChunk" || stage == "progress.recognizeCloud" || stage == "progress.recognizedChunks"
    // Rust progress keys: chunk stages report the 0-based chunk index while it
    // is in flight and the completed count afterwards.
    private fun describe(stage: String, current: Long, total: Long): String = when {
        chunked(stage) && total > 1 -> {
            val part = (current + 1).coerceIn(1, total)
            val step = getString(if(stage == "progress.sendCloudChunk") R.string.hearfolio_operation_step_send else R.string.hearfolio_operation_step_recognize)
            getString(R.string.hearfolio_operation_chunk, part.toInt(), total.toInt(), step)
        }
        stage == "progress.sendCloudChunk" || stage == "progress.sendCloudAudio" -> getString(R.string.hearfolio_operation_send)
        chunked(stage) || stage == "progress.recognizeWhisper" || stage == "progress.streamRecognition" || stage == "progress.finishRecognition" -> getString(R.string.hearfolio_operation_recognize)
        stage == "progress.prepareAudio" -> getString(R.string.hearfolio_operation_prepare)
        stage == "progress.done" -> getString(R.string.hearfolio_operation_save)
        stage == "progress.postprocess" -> getString(R.string.hearfolio_operation_postprocess)
        stage == "progress.copyAudio" -> getString(R.string.hearfolio_operation_copy)
        else -> getString(R.string.hearfolio_operation_working)
    }
    private fun notification(stage: String = "", current: Long = 0, total: Long = 0, ongoing: Boolean = true): Notification {
        val open = PendingIntent.getActivity(this, NOTIFICATION, mainAppIntent(this), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val title = getString(when(kind) {
            "postprocess" -> R.string.hearfolio_operation_title_postprocess
            "import" -> R.string.hearfolio_operation_title_import
            else -> R.string.hearfolio_operation_title_transcribe
        })
        val text = when(stage) {
            "finished" -> getString(R.string.hearfolio_operation_finished)
            "cancelled" -> getString(R.string.hearfolio_operation_cancelled)
            "cancelling" -> getString(R.string.hearfolio_operation_cancelling)
            "error", "interrupted" -> getString(R.string.hearfolio_operation_failed)
            else -> describe(stage,current,total)
        }
        val b = NotificationCompat.Builder(this, CHANNEL).setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(title).setContentText(text).setContentIntent(open).setOngoing(ongoing)
            .setOnlyAlertOnce(true).setAutoCancel(!ongoing).setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setCategory(NotificationCompat.CATEGORY_PROGRESS)
        if(startedAt > 0) b.setWhen(startedAt).setShowWhen(true).setUsesChronometer(ongoing)
        if(ongoing) {
            val cancel = PendingIntent.getService(this, NOTIFICATION, Intent(this, OperationService::class.java).setAction(CANCEL).putExtra("id", job), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            if(stage != "cancelling") b.addAction(0, getString(R.string.hearfolio_operation_cancel), cancel)
            when {
                stage != "cancelling" && chunked(stage) && total > 1 -> b.setProgress(total.toInt(), current.coerceIn(0,total).toInt(), false)
                stage == "progress.prepareAudio" && total > 0 && current in 0..total -> b.setProgress(100, (100.0 * current / total).toInt().coerceIn(0,100), false)
                else -> b.setProgress(0, 0, true)
            }
        }
        return b.build()
    }
    @Suppress("DEPRECATION")
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if(intent == null) { stopSelf(); return START_NOT_STICKY }
        val id = intent.getStringExtra("id") ?: run { stopSelf(); return START_NOT_STICKY }
        if(intent.action == CANCEL) { if(id == job) { cancelled = true; error = "errors.operationCancelled"; persistControl(); update("cancelling",0,0) }; return START_NOT_STICKY }
        if(intent.action != BEGIN) return START_NOT_STICKY
        try {
            releaseLocks()
            job = id; cancelled = false; error = null; lastProgress = null; startedAt = System.currentTimeMillis()
            kind = intent.getStringExtra("kind") ?: "transcribe"
            snapshotFile=privateStateFile(intent.getStringExtra("snapshotPath"));controlFile=privateStateFile(intent.getStringExtra("controlPath"));persistControl()
            if(Build.VERSION.SDK_INT >= 29) startForeground(NOTIFICATION,notification(),ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
            else startForeground(NOTIFICATION,notification())
            val cpu = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:operation").apply { setReferenceCounted(false) }
            // HIGH_PERF is deprecated (a no-op on API 34+); LOW_LATENCY replaces it from API 29.
            val wifiMode = if(Build.VERSION.SDK_INT >= 29) WifiManager.WIFI_MODE_FULL_LOW_LATENCY else WifiManager.WIFI_MODE_FULL_HIGH_PERF
            val network = if(intent.getBooleanExtra("network", false)) (applicationContext.getSystemService(WIFI_SERVICE) as WifiManager)
                .createWifiLock(wifiMode,"Hearfolio:operation").apply { setReferenceCounted(false) } else null
            resources = OperationResources(object : OperationLock {
                override fun acquire(timeoutMillis: Long) { cpu.acquire(timeoutMillis) }
                override fun release() { if(cpu.isHeld) cpu.release() }
            }, network?.let { lock -> object : OperationLock {
                override fun acquire(timeoutMillis: Long) { lock.acquire() }
                override fun release() { if(lock.isHeld) lock.release() }
            } }).also { it.acquire(MAX_MILLIS) }
            handler.postDelayed(deadline,MAX_MILLIS)
            armStall()
            handler.post(poll)
            resolveReady(id, null)
        } catch(_: Exception) { error = "errors.operationBackgroundUnavailable"; cancelled = true; resolveReady(id, error); stopSelf() }
        return START_NOT_STICKY
    }
    fun update(stage: String, current: Long, total: Long) {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION,notification(if(cancelled) "cancelling" else stage,current,total))
    }
    fun finish(success: Boolean) = finish(if(success) "finished" else "error")
    // `state` is the terminal snapshot state written by Rust.
    fun finish(state: String) {
        stopForeground(STOP_FOREGROUND_REMOVE)
        val shown = if(state == "cancelled" || (state != "finished" && error == "errors.operationCancelled")) "cancelled" else state
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION,notification(shown,ongoing=false))
        releaseLocks(); stopSelf()
    }
    private fun abort(reason: String) { cancelled = true; error = reason; try { persistControl() } finally { finish("error") } }
    private fun releaseLocks() { handler.removeCallbacks(deadline); handler.removeCallbacks(stall); handler.removeCallbacks(poll); resources?.close(); resources = null }
    override fun onTimeout(startId: Int, fgsType: Int) { abort("errors.operationTimeout") }
    override fun onDestroy() { releaseLocks(); cancelled = true; if(error == null) error = "errors.operationInterrupted"; try { persistControl() } catch(_:Exception) {}; instance = null; super.onDestroy() }
    override fun onBind(intent: Intent?): IBinder? = null
}
