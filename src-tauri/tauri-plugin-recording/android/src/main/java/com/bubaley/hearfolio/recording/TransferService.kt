package com.bubaley.hearfolio.recording

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import java.util.Locale
import java.io.File
import org.json.JSONObject

// The Rust transfer owns the network operation. This service owns its Android
// lifetime and notification, independent of the activity/WebView lifecycle.
class TransferService : Service() {
    companion object {
        const val CHANNEL = "hearfolio-transfers"
        const val NOTIFICATION = 4102
        const val BEGIN = "hearfolio.transfer.BEGIN"
        const val CANCEL = "hearfolio.transfer.CANCEL"
        const val END = "hearfolio.transfer.END"
        @Volatile var job: String? = null
        @Volatile var cancelled = false
        @Volatile var instance: TransferService? = null
        var ready: ((String?) -> Unit)? = null
        var stopped: (() -> Unit)? = null
        fun prepare(id: String) { job = id; cancelled = false }
        fun isCancelled(id: String) = job != id || cancelled
    }
    private var wake: PowerManager.WakeLock? = null
    private var wifi: WifiManager.WifiLock? = null
    private var stateFile: File? = null
    private var controlFile: File? = null
    private val poll = object : Runnable {
        override fun run() {
            val control=NativeJobJournal.read(controlFile)
            if(control!=null && control.optString("id")==job && control.optBoolean("cancelled")) cancelled=true
            val state=NativeJobJournal.read(stateFile)
            if(state != null && state.optString("id") == job) {
                if(state.optString("state") != "running") { complete(state.optBoolean("success"));return }
                update(state.optString("stage"),state.optLong("current"),state.optLong("total"))
            }
            handler.postDelayed(this,1000)
        }
    }
    private fun control() { try { NativeJobJournal.write(controlFile,JSONObject().put("id",job).put("ready",true).put("cancelled",cancelled)) } catch(_:Exception) {} }
    private fun complete(ok: Boolean) {
        success=ok;stopForeground(STOP_FOREGROUND_REMOVE)
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION, notification(if(ok) tr("Передача завершена","Transfer completed") else if(cancelled) tr("Передача отменена","Transfer cancelled") else tr("Передача прервана. Повторите в приложении.","Transfer interrupted. Retry in the app."),ongoing=false))
        releaseLocks();stopSelf()
    }
    private var title = "Hearfolio"
    private var success = false
    private val handler = Handler(Looper.getMainLooper())
    private val ru get() = Locale.getDefault().language == "ru"
    private fun tr(russian: String, english: String) = if (ru) russian else english
    private val deadline = Runnable { cancelled = true;control();complete(false) }

    override fun onCreate() {
        super.onCreate(); instance = this
        if (Build.VERSION.SDK_INT >= 26) {
            val channel = NotificationChannel(CHANNEL, tr("Передача записей", "Recording transfers"), NotificationManager.IMPORTANCE_LOW)
            channel.lockscreenVisibility = Notification.VISIBILITY_PUBLIC
            getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }
    }

    private fun notification(text: String, current: Long = 0, total: Long = 0, ongoing: Boolean = true): Notification {
        val open = mainAppIntent(this).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val content = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val builder = NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_upload).setContentTitle(title)
            .setContentText(text).setContentIntent(content).setOnlyAlertOnce(true)
            .setOngoing(ongoing).setAutoCancel(!ongoing).setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
        if (ongoing) {
            val cancel = PendingIntent.getService(this, 1, Intent(this, TransferService::class.java).setAction(CANCEL).putExtra("id", job), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
            builder.addAction(android.R.drawable.ic_menu_close_clear_cancel, tr("Отменить", "Cancel"), cancel)
            builder.setProgress(100, if (total > 0) (100.0 * current / total).toInt().coerceIn(0, 100) else 0, total <= 0)
        }
        return builder.build()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent == null || intent.getStringExtra("id") != job) return START_NOT_STICKY
        when (intent.action) {
            CANCEL -> { cancelled = true; control(); update("cancelled", 0, 0) }
            END -> complete(intent.getBooleanExtra("success",false))
            BEGIN -> {
                try {
                releaseLocks()
                stateFile=NativeJobJournal.privateFile(this,intent.getStringExtra("snapshotPath"));controlFile=NativeJobJournal.privateFile(this,intent.getStringExtra("controlPath"));control()
                title = if (intent.getStringExtra("direction") == "receive") tr("Получение записи", "Receiving recording") else tr("Отправка записи", "Sending recording")
                val notification = notification(tr("Подготовка…", "Preparing…"))
                if (Build.VERSION.SDK_INT >= 29) startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
                else startForeground(NOTIFICATION, notification)
                wake = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Hearfolio:transfer").apply { setReferenceCounted(false); acquire(2 * 60 * 60 * 1000L) }
                @Suppress("DEPRECATION")
                wifi = (applicationContext.getSystemService(WIFI_SERVICE) as WifiManager).createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "Hearfolio:transfer").apply { setReferenceCounted(false); acquire() }
                // Bound locks and service lifetime even if a native job never completes.
                handler.postDelayed(deadline, 2 * 60 * 60 * 1000L)
                handler.post(poll)
                ready?.invoke(null); ready = null
                } catch (error: Exception) {
                    cancelled = true
                    ready?.invoke("errors.transferBackgroundUnavailable"); ready = null
                    stopSelf()
                }
            }
        }
        return START_NOT_STICKY
    }

    fun update(stage: String, current: Long, total: Long) {
        val text = when (if(cancelled) "cancelled" else stage) {
            "waiting" -> tr("Ожидание получателя…", "Waiting for recipient…")
            "cancelled" -> tr("Отменяем передачу…", "Cancelling transfer…")
            "metadata" -> tr("Обновление расшифровки…", "Updating transcription…")
            else -> if (total > 0) "${(100.0 * current / total).toInt().coerceIn(0, 100)}%" else tr("Подготовка…", "Preparing…")
        }
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION, notification(text, current, total))
    }

    override fun onTimeout(startId: Int, fgsType: Int) { cancelled = true; control();complete(false) }
    private fun releaseLocks() {
        handler.removeCallbacks(deadline);handler.removeCallbacks(poll)
        if (wake?.isHeld == true) wake?.release()
        if (wifi?.isHeld == true) wifi?.release()
        wake=null;wifi=null
    }
    override fun onDestroy() {
        releaseLocks()
        cancelled = true;control(); instance = null
        super.onDestroy()
        stopped?.invoke(); stopped = null
    }
    override fun onBind(intent: Intent?): IBinder? = null
}
