package com.bubaley.hearfolio.recording

import android.app.*
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.*
import androidx.core.app.NotificationCompat
import java.util.Locale
import java.io.File
import org.json.JSONObject
import android.net.wifi.WifiManager

// Explicitly enabled connection to the user's paired external devices.
// Tokens and network/file operations stay in Rust, never in notifications.
class InboxService : Service() {
    companion object {
        const val START = "hearfolio.inbox.START"
        const val STOP = "hearfolio.inbox.STOP"
        const val CHANNEL = "hearfolio-inbox"
        const val ID = 4105
        @Volatile var failure: String? = null
        @Volatile var active = false
        @Volatile var instance: InboxService? = null
        var ready: ((String?) -> Unit)? = null
    }
    private var receiving = true
    private val handler=Handler(Looper.getMainLooper())
    private var stateFile: File?=null
    private var offersFile: File?=null
    private var transferFile: File?=null
    private var transferState: File?=null
    private var transferControl: File?=null
    private var transferId: String?=null
    private var revision=0L
    private var transferStarted=0L
    private var wifi: WifiManager.WifiLock?=null
    private fun status() { try { NativeJobJournal.write(stateFile,JSONObject().put("active",active).put("error",failure)) } catch(_:Exception) {} }
    private fun transferStatus(cancelled: Boolean) { try { NativeJobJournal.write(transferControl,JSONObject().put("id",transferId).put("ready",true).put("cancelled",cancelled)) } catch(_:Exception) {} }
    private fun interruptWaiting() {
        failure="errors.transferBackgroundUnavailable";active=false;status();TransferService.cancelled=true;transferStatus(true)
        stopForeground(STOP_FOREGROUND_REMOVE)
        getSystemService(NotificationManager::class.java).notify(ID,NotificationCompat.Builder(this,CHANNEL).setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("Hearfolio").setContentText(tr("Ожидание остановлено. Откройте приложение, чтобы продолжить.","Waiting stopped. Open the app to continue."))
            .setContentIntent(open()).setAutoCancel(true).setOngoing(false).build())
        stopSelf()
    }
    private val deadline=Runnable { interruptWaiting() }
    private val poll=object:Runnable {
        override fun run() {
            val offerState=NativeJobJournal.read(offersFile)
            if(offerState!=null && offerState.optLong("revision")!=revision) { revision=offerState.optLong("revision");offers(offerState.optInt("current"),offerState.optBoolean("success")) }
            val request=NativeJobJournal.read(transferFile)
            val requestedId=request?.optString("id")
            if(request!=null && requestedId!=null && requestedId!=transferId) try {
                transferId=requestedId
                transferState=NativeJobJournal.privateFile(this@InboxService,request.optString("snapshotPath"));transferControl=NativeJobJournal.privateFile(this@InboxService,request.optString("controlPath"))
                transferStarted=SystemClock.elapsedRealtime();transferStatus(false)
                transfer(true,false,request.optString("direction"))
            } catch(_:Exception) { transferStatus(true) }
            val task=NativeJobJournal.read(transferState)
            if(task!=null && task.optString("id")==transferId && transferStarted>0) {
                if(task.optString("state") != "running") { transfer(false,task.optBoolean("success"));transferStarted=0 }
                else if(SystemClock.elapsedRealtime()-transferStarted>=2*60*60*1000L) { transferStatus(true);transfer(false,false);transferStarted=0 }
                else progress(task.optString("stage"),task.optLong("current"),task.optLong("total"))
            }
            handler.postDelayed(this,1000)
        }
    }
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
        if (intent?.action == STOP) { TransferService.cancelled = true;transferStatus(true); stopSelf(); return START_NOT_STICKY }
        if (intent?.action != START) { stopSelf(); return START_NOT_STICKY }
        try {
            stateFile=NativeJobJournal.privateFile(this,intent.getStringExtra("snapshotPath"));offersFile=NativeJobJournal.privateFile(this,intent.getStringExtra("controlPath"));transferFile=File(stateFile!!.parentFile,"inbox-transfer.json")
            // Do not replay an old transfer command on a newly enabled inbox.
            if(!active) transferFile?.delete();
            val n = notification(tr("Ожидание записей от ваших устройств","Waiting for recordings from your devices"))
            if (Build.VERSION.SDK_INT >= 29) startForeground(ID,n,ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE or (if(transferStarted>0) ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC else 0)) else startForeground(ID,n)
            if(wake?.isHeld == true) wake?.release()
            wake = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:inbox").apply { setReferenceCounted(false); acquire(12*60*60*1000L) }
            failure=null;active = true;status();handler.removeCallbacks(poll);handler.post(poll);handler.removeCallbacks(deadline);handler.postDelayed(deadline,12*60*60*1000L);ready?.invoke(null); ready = null
        } catch (_: Exception) { failure="errors.transferBackgroundUnavailable";active = false;status(); ready?.invoke("errors.transferBackgroundUnavailable"); ready = null; stopSelf() }
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
        if(Build.VERSION.SDK_INT>=29) startForeground(ID,notification(tr("Связь с устройствами","Device connection")),ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE or (if(running) ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC else 0))
        @Suppress("DEPRECATION")
        if(running && wifi==null) wifi=(applicationContext.getSystemService(WIFI_SERVICE) as WifiManager).createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF,"Hearfolio:inbox-transfer").apply {setReferenceCounted(false);acquire()}
        if(!running) {if(wifi?.isHeld==true)wifi?.release();wifi=null}

        getSystemService(NotificationManager::class.java).notify(ID,notification(if (running) { if (receiving) tr("Получение записи…","Receiving recording…") else tr("Отправка записи…","Sending recording…") } else tr("Ожидание записей от ваших устройств","Waiting for recordings from your devices")))
        if (!running && success) getSystemService(NotificationManager::class.java).notify(ID + 2,NotificationCompat.Builder(this,"hearfolio-incoming").setSmallIcon(android.R.drawable.stat_sys_download).setContentTitle("Hearfolio").setContentText(tr("Запись сохранена","Recording saved")).setContentIntent(open()).setAutoCancel(true).build())
    }
    fun progress(stage: String, current: Long, total: Long) {
        val text = if (total > 0) "${(100.0 * current / total).toInt().coerceIn(0,100)}%" else if (stage == "waiting") tr("Ожидание получателя…","Waiting for recipient…") else tr("Передача записи…","Transferring recording…")
        getSystemService(NotificationManager::class.java).notify(ID,notification(text))
    }
    override fun onDestroy() {
        handler.removeCallbacks(poll);handler.removeCallbacks(deadline)
        active = false;status();transferStatus(true);instance = null
        if(wifi?.isHeld==true)wifi?.release();wifi=null
        if (TransferService.instance == null) TransferService.cancelled = true
        if (wake?.isHeld == true) wake?.release()
        getSystemService(NotificationManager::class.java).cancel(ID + 1)
        super.onDestroy()
    }
    override fun onTimeout(startId:Int,fgsType:Int) {interruptWaiting()}
    override fun onBind(intent: Intent?): IBinder? = null
}
