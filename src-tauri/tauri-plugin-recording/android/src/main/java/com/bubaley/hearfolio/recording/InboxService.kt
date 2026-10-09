package com.bubaley.hearfolio.recording

import android.app.*
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.*
import androidx.core.app.NotificationCompat
import java.util.Locale

// Explicitly enabled connection to the user's paired external devices.
// Tokens and network/file operations stay in Rust, never in notifications.
class InboxService : Service() {
    companion object {
        const val START = "hearfolio.inbox.START"
        const val STOP = "hearfolio.inbox.STOP"
        const val CHANNEL = "hearfolio-inbox"
        const val ID = 4104
        @Volatile var active = false
        @Volatile var instance: InboxService? = null
        var ready: ((String?) -> Unit)? = null
    }
    private var receiving = true
    private var wake: PowerManager.WakeLock? = null
    private fun tr(ru: String, en: String) = if (Locale.getDefault().language == "ru") ru else en
    private fun open() = PendingIntent.getActivity(this, ID,mainAppIntent(this),PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    private fun notification(text: String): Notification {
        val stop = PendingIntent.getService(this,ID,Intent(this,InboxService::class.java).setAction(STOP),PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        return NotificationCompat.Builder(this,CHANNEL).setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("Hearfolio").setContentText(text).setContentIntent(open()).setOngoing(true).setOnlyAlertOnce(true)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC).addAction(0,tr("Выключить", "Turn off"),stop).build()
    }
    override fun onCreate() {
        super.onCreate(); instance = this
        if (Build.VERSION.SDK_INT >= 26) {
            getSystemService(NotificationManager::class.java).createNotificationChannel(NotificationChannel(CHANNEL,tr("Связь с устройствами","Device connection"),NotificationManager.IMPORTANCE_LOW))
            getSystemService(NotificationManager::class.java).createNotificationChannel(NotificationChannel("hearfolio-incoming",tr("Входящие записи","Incoming recordings"),NotificationManager.IMPORTANCE_DEFAULT))
        }
    }
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == STOP) { TransferService.cancelled = true; stopSelf(); return START_NOT_STICKY }
        if (intent?.action != START) { stopSelf(); return START_NOT_STICKY }
        try {
            val n = notification(tr("Ожидание записей от ваших устройств","Waiting for recordings from your devices"))
            if (Build.VERSION.SDK_INT >= 29) startForeground(ID,n,ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE) else startForeground(ID,n)
            if (!active) wake = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:inbox").apply { setReferenceCounted(false); acquire() }
            active = true; ready?.invoke(null); ready = null
        } catch (_: Exception) { active = false; ready?.invoke("errors.transferBackgroundUnavailable"); ready = null; stopSelf() }
        return START_NOT_STICKY
    }
    fun offers(count: Int, fresh: Boolean) {
        if (!active) return
        val manager = getSystemService(NotificationManager::class.java)
        if (count == 0) { manager.cancel(ID + 1); return }
        if (fresh) manager.notify(ID + 1,NotificationCompat.Builder(this,"hearfolio-incoming").setSmallIcon(android.R.drawable.stat_sys_download).setContentTitle("Hearfolio")
            .setContentText(tr("Новая запись или запрос подключения. Откройте приложение.","New recording or pairing request. Open the app."))
            .setContentIntent(open()).setAutoCancel(true).setVisibility(NotificationCompat.VISIBILITY_PUBLIC).build())
    }
    fun transfer(running: Boolean, success: Boolean, direction: String = "receive") {
        receiving = direction == "receive"
        getSystemService(NotificationManager::class.java).notify(ID,notification(if (running) { if (receiving) tr("Получение записи…","Receiving recording…") else tr("Отправка записи…","Sending recording…") } else tr("Ожидание записей от ваших устройств","Waiting for recordings from your devices")))
        if (!running && success) getSystemService(NotificationManager::class.java).notify(ID + 2,NotificationCompat.Builder(this,"hearfolio-incoming").setSmallIcon(android.R.drawable.stat_sys_download).setContentTitle("Hearfolio").setContentText(tr("Запись сохранена","Recording saved")).setContentIntent(open()).setAutoCancel(true).build())
    }
    fun progress(stage: String, current: Long, total: Long) {
        val text = if (total > 0) "${(100.0 * current / total).toInt().coerceIn(0,100)}%" else if (stage == "waiting") tr("Ожидание получателя…","Waiting for recipient…") else tr("Передача записи…","Transferring recording…")
        getSystemService(NotificationManager::class.java).notify(ID,notification(text))
    }
    override fun onDestroy() {
        active = false; instance = null
        if (TransferService.instance == null) TransferService.cancelled = true
        if (wake?.isHeld == true) wake?.release()
        getSystemService(NotificationManager::class.java).cancel(ID + 1)
        super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
}
