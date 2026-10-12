package com.bubaley.hearfolio.recording

import android.app.*
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.*
import androidx.core.app.NotificationCompat
import java.util.Locale
import java.io.File
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
        const val MAX_MILLIS = 2 * 60 * 60 * 1000L
        @Volatile var instance: OperationService? = null
        @Volatile var job: String? = null
        @Volatile var cancelled = false
        @Volatile var error: String? = null
        var ready: ((String?) -> Unit)? = null
        fun isCancelled(id: String) = job != id || cancelled || instance == null
    }
    private var resources: OperationResources? = null
    private val handler = Handler(Looper.getMainLooper())
    private var kind = "transcribe"
    private var snapshotFile: File? = null
    private var controlFile: File? = null
    private val poll = object : Runnable {
        override fun run() {
            try {
                val snapshot=JSONObject(requireNotNull(snapshotFile).readText())
                if(snapshot.optString("id") == job) {
                    val state=snapshot.optString("state")
                    if(state != "running") { finish(state == "finished");return }
                    if(snapshot.optString("error") == "errors.operationCancelled") { cancelled=true;error="errors.operationCancelled";persistControl() }
                    val progress=snapshot.optJSONObject("progress")
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
    private fun tr(ru: String, en: String) = if (Locale.getDefault().language == "ru") ru else en
    override fun onCreate() {
        super.onCreate(); instance = this
        if (Build.VERSION.SDK_INT >= 26) getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, tr("Обработка записей", "Recording processing"), NotificationManager.IMPORTANCE_LOW))
    }
    private fun notification(stage: String = "", current: Long = 0, total: Long = 0, ongoing: Boolean = true): Notification {
        val open = PendingIntent.getActivity(this, NOTIFICATION, mainAppIntent(this), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val title = when(kind) {
            "postprocess" -> tr("Обработка текста", "Processing text")
            "import" -> tr("Сохранение записи", "Saving recording")
            else -> tr("Расшифровка записи", "Transcribing recording")
        }
        val text = when(stage) {
            "finished" -> tr("Готово", "Completed")
            "cancelled" -> tr("Отменяем…", "Cancelling…")
            "error" -> tr("Обработка прервана. Откройте приложение.", "Processing interrupted. Open the app.")
            else -> if(total > 0) "${(100.0 * current / total).toInt().coerceIn(0,100)}%" else tr("Идёт обработка…", "Processing…")
        }
        val b = NotificationCompat.Builder(this, CHANNEL).setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle(title).setContentText(text).setContentIntent(open).setOngoing(ongoing)
            .setOnlyAlertOnce(true).setAutoCancel(!ongoing).setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
        if(ongoing) {
            val cancel = PendingIntent.getService(this, NOTIFICATION, Intent(this, OperationService::class.java).setAction(CANCEL).putExtra("id", job), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            b.addAction(0, tr("Отменить", "Cancel"), cancel)
            b.setProgress(100, if(total > 0) (100.0 * current / total).toInt().coerceIn(0,100) else 0, total <= 0)
        }
        return b.build()
    }
    @Suppress("DEPRECATION")
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if(intent == null) { stopSelf(); return START_NOT_STICKY }
        val id = intent.getStringExtra("id") ?: run { stopSelf(); return START_NOT_STICKY }
        if(intent.action == CANCEL) { if(id == job) { cancelled = true; error = "errors.operationCancelled"; persistControl(); update("cancelled",0,0) }; return START_NOT_STICKY }
        if(intent.action != BEGIN) return START_NOT_STICKY
        try {
            releaseLocks()
            job = id; cancelled = false; error = null
            kind = intent.getStringExtra("kind") ?: "transcribe"
            snapshotFile=privateStateFile(intent.getStringExtra("snapshotPath"));controlFile=privateStateFile(intent.getStringExtra("controlPath"));persistControl()
            if(Build.VERSION.SDK_INT >= 29) startForeground(NOTIFICATION,notification(),ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
            else startForeground(NOTIFICATION,notification())
            val cpu = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:operation").apply { setReferenceCounted(false) }
            val network = if(intent.getBooleanExtra("network", false)) (applicationContext.getSystemService(WIFI_SERVICE) as WifiManager)
                .createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF,"Hearfolio:operation").apply { setReferenceCounted(false) } else null
            resources = OperationResources(object : OperationLock {
                override fun acquire(timeoutMillis: Long) { cpu.acquire(timeoutMillis) }
                override fun release() { if(cpu.isHeld) cpu.release() }
            }, network?.let { lock -> object : OperationLock {
                override fun acquire(timeoutMillis: Long) { lock.acquire() }
                override fun release() { if(lock.isHeld) lock.release() }
            } }).also { it.acquire(MAX_MILLIS) }
            handler.postDelayed(deadline,MAX_MILLIS)
            handler.post(poll)
            ready?.invoke(null); ready = null
        } catch(_: Exception) { error = "errors.operationBackgroundUnavailable"; cancelled = true; ready?.invoke(error); ready = null; stopSelf() }
        return START_NOT_STICKY
    }
    fun update(stage: String, current: Long, total: Long) {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION,notification(if(cancelled) "cancelled" else stage,current,total))
    }
    fun finish(success: Boolean) {
        stopForeground(STOP_FOREGROUND_REMOVE)
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION,notification(if(success) "finished" else "error",ongoing=false))
        releaseLocks(); stopSelf()
    }
    private fun abort(reason: String) { cancelled = true; error = reason; try { persistControl() } finally { finish(false) } }
    private fun releaseLocks() { handler.removeCallbacks(deadline); handler.removeCallbacks(poll); resources?.close(); resources = null }
    override fun onTimeout(startId: Int, fgsType: Int) { abort("errors.operationTimeout") }
    override fun onDestroy() { releaseLocks(); cancelled = true; if(error == null) error = "errors.operationInterrupted"; try { persistControl() } catch(_:Exception) {}; instance = null; super.onDestroy() }
    override fun onBind(intent: Intent?): IBinder? = null
}
