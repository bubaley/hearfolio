package com.bubaley.hearfolio.recording

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import java.util.Locale

// Keeps the process (and the Rust upload worker thread) alive while cloud
// uploads run in the background. Rust owns the queue and calls `update`.
class UploadService : Service() {
    companion object {
        const val CHANNEL = "hearfolio-uploads"
        const val NOTIFICATION = 4103
        const val UPDATE = "hearfolio.upload.UPDATE"
        @Volatile var running = false

        fun update(context: Context, count: Int, current: Long, total: Long) {
            val app = context.applicationContext
            if (count <= 0) {
                if (running) app.stopService(Intent(app, UploadService::class.java))
                return
            }
            val intent = Intent(app, UploadService::class.java).setAction(UPDATE)
                .putExtra("count", count).putExtra("current", current).putExtra("total", total)
            try {
                if (running) app.startService(intent) else ContextCompat.startForegroundService(app, intent)
            } catch (_: Exception) {
                // Android 12+ forbids starting from the background: the queue
                // simply waits until the app is opened again.
            }
        }
    }
    private var wake: PowerManager.WakeLock? = null
    private val ru get() = Locale.getDefault().language == "ru"
    private fun tr(russian: String, english: String) = if (ru) russian else english

    override fun onCreate() {
        super.onCreate()
        if (Build.VERSION.SDK_INT >= 26) {
            val channel = NotificationChannel(CHANNEL, tr("Загрузка в облако", "Cloud uploads"), NotificationManager.IMPORTANCE_LOW)
            getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }
    }

    private fun notification(count: Int, current: Long, total: Long): Notification {
        val open = PendingIntent.getActivity(this, 0, mainAppIntent(this), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val percent = if (total > 0) (100.0 * current / total).toInt().coerceIn(0, 100) else 0
        val title = if (count == 1) tr("Загружаем запись", "Uploading recording") else tr("Загружаем записи: $count", "Uploading $count recordings")
        return NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_upload).setContentTitle(title)
            .setContentText("$percent%").setContentIntent(open).setOnlyAlertOnce(true).setOngoing(true)
            .setProgress(100, percent, total <= 0).build()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val count = intent?.getIntExtra("count", 0) ?: 0
        val note = notification(count.coerceAtLeast(1), intent?.getLongExtra("current", 0) ?: 0, intent?.getLongExtra("total", 0) ?: 0)
        try {
            if (Build.VERSION.SDK_INT >= 29) startForeground(NOTIFICATION, note, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
            else startForeground(NOTIFICATION, note)
            running = true
            if (wake == null) wake = (getSystemService(POWER_SERVICE) as PowerManager)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Hearfolio:upload").apply { setReferenceCounted(false); acquire(2 * 60 * 60 * 1000L) }
        } catch (_: Exception) { stopSelf() }
        if (count <= 0) stopSelf()
        return START_NOT_STICKY
    }

    override fun onTimeout(startId: Int, fgsType: Int) { stopSelf() }
    override fun onDestroy() {
        running = false
        if (wake?.isHeld == true) wake?.release()
        wake = null
        super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
}
