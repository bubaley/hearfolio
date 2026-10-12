package com.bubaley.hearfolio.recording

import android.app.*
import android.content.Intent
import android.content.Context
import android.content.pm.ServiceInfo
import android.os.*
import androidx.core.app.NotificationCompat
import java.io.File
import java.util.Locale
import org.json.JSONObject
import android.util.AtomicFile

// Own the microphone independently of the activity. Start is only requested by
// the visible app; notification actions control an already running recording.
class RecordingService : Service() {
    companion object {
        const val START = "hearfolio.recording.START"
        const val STOP = "hearfolio.recording.STOP"
        const val PAUSE = "hearfolio.recording.PAUSE"
        const val RESUME = "hearfolio.recording.RESUME"
        const val CHANNEL = "hearfolio-recording"
        const val ID = 4103
        @Volatile var instance: RecordingService? = null
        @Volatile var state = "idle"
        @Volatile var error: String? = null
        @Volatile var interrupted = false
        @Volatile var recoveredElapsed = 0L
        @Volatile var inputLevel = 0f
        @Volatile var quickRecording = false
        @Volatile var path = ""
        @Volatile var startedAt = 0L
        @Volatile var pausedAt = 0L
        @Volatile var pausedMillis = 0L
        fun elapsedSeconds() = if (startedAt == 0L) recoveredElapsed else ((if (pausedAt > 0L) pausedAt else SystemClock.elapsedRealtime()) - startedAt - pausedMillis).coerceAtLeast(0L) / 1000L
        var ready: ((String?) -> Unit)? = null
        private fun preferences(context: Context) = context.getSharedPreferences("hearfolio-recording", Context.MODE_PRIVATE)
        fun persist(context: Context) {
            if(path.isNotEmpty() && !quickRecording) try {
                val atomic=AtomicFile(File(path+".state.json"));val stream=atomic.startWrite()
                try { stream.write(JSONObject().put("state",state).put("path",path).put("error",error).put("quick",quickRecording).put("elapsedSeconds",elapsedSeconds()).put("updatedAt",System.currentTimeMillis()).toString().toByteArray());atomic.finishWrite(stream) }
                catch(error:Exception) {atomic.failWrite(stream)}
            } catch(_:Exception) {}
            preferences(context).edit().putString("state",state).putString("path",path).putBoolean("quick",quickRecording)
                .putLong("elapsed",elapsedSeconds()).putString("error",error).putBoolean("interrupted",interrupted).commit()
        }
        fun acknowledge(context: Context) { state = "idle"; path = ""; error = null; interrupted = false; recoveredElapsed = 0L; startedAt = 0L; persist(context) }
        fun restore(context: Context) {
            if(instance != null) return
            if(path.isNotEmpty()) { if(state=="finished" && !File(path).exists()) acknowledge(context);return }
            val saved=preferences(context)
            path=saved.getString("path", "") ?: ""
            state=saved.getString("state","idle") ?: "idle"
            quickRecording=saved.getBoolean("quick",false)
            error=saved.getString("error",null);interrupted=saved.getBoolean("interrupted",false)
            recoveredElapsed=saved.getLong("elapsed",0);startedAt=0L
            if(path.isEmpty()) return
            val file=try { File(path).canonicalFile } catch(_: Exception) { acknowledge(context); return }
            if(!file.path.startsWith(context.dataDir.canonicalPath+File.separator) || !file.name.startsWith("hearfolio-recording-")) { acknowledge(context); return }
            if(!file.exists()) { acknowledge(context); return }
            if(state == "recording" || state == "paused") {
                interrupted=true
                val recovered=WavRecovery.recover(file)
                if(recovered != null) {
                    recoveredElapsed=recovered;state="finished";error="errors.recordingInterrupted"
                    if(quickRecording && file.name.endsWith(".partial")) {
                        val target=File(path.removeSuffix(".partial"))
                        if(file.renameTo(target)) path=target.path else {state="error";error="errors.recordingStop"}
                    }
                } else { state="error";error="errors.recordingInterrupted" }
                persist(context)
            }
        }
    }
    private var recorder: RecorderSession? = null
    private var wake: PowerManager.WakeLock? = null
    private val handler = Handler(Looper.getMainLooper())
    private val deadline = Runnable { finish(true) }
    // Both interfaces consume the same envelope from the capture session.
    private val sampleLevel = object : Runnable {
        override fun run() {
            inputLevel = if (state == "recording") recorder?.level ?: 0f else 0f
            if (recorder != null) handler.postDelayed(this, 100)
        }
    }
    private fun resetLevel() { handler.removeCallbacks(sampleLevel); inputLevel = 0f }
    private fun tr(ru: String, en: String) = if (Locale.getDefault().language == "ru") ru else en
    private fun action(action: String): PendingIntent = PendingIntent.getService(this, action.hashCode(), Intent(this, RecordingService::class.java).setAction(action).putExtra("path",path), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    private fun notification(done: Boolean = false): Notification {
        val open = PendingIntent.getActivity(this, ID, mainAppIntent(this), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val b = NotificationCompat.Builder(this, CHANNEL).setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle("Hearfolio").setContentText(if (done) tr("Аудио записано", "Audio recorded") else if (state == "paused") tr("Запись на паузе", "Recording paused") else tr("Идёт запись", "Recording in progress"))
            .setContentIntent(open).setOnlyAlertOnce(true).setOngoing(!done).setAutoCancel(done)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC).setCategory(NotificationCompat.CATEGORY_TRANSPORT)
        if (!done) {
            b.addAction(0, if (state == "paused") tr("Продолжить", "Resume") else tr("Пауза", "Pause"), action(if (state == "paused") RESUME else PAUSE))
            b.addAction(0, tr("Сохранить", "Save"), action(STOP))
        }
        return b.build()
    }
    override fun onCreate() {
        super.onCreate(); instance = this
        if (Build.VERSION.SDK_INT >= 26) getSystemService(NotificationManager::class.java).createNotificationChannel(NotificationChannel(CHANNEL,tr("Запись аудио","Audio recording"),NotificationManager.IMPORTANCE_LOW).apply { lockscreenVisibility = Notification.VISIBILITY_PUBLIC })
    }
    @Suppress("DEPRECATION")
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent == null) { stopSelf(); return START_NOT_STICKY }
        if (intent.action != START && intent.getStringExtra("path") != path) return START_NOT_STICKY
        when (intent.action) {
            START -> {
                if (recorder != null) { ready?.invoke("errors.operationBusy");ready=null;return START_NOT_STICKY }
                var allocated: File? = null
                try {
                quickRecording = intent.getBooleanExtra("quick",false)
                val file = File(intent.getStringExtra("path") ?: throw IllegalArgumentException()).canonicalFile
                if (!file.path.startsWith(dataDir.canonicalPath + File.separator) || !file.name.startsWith("hearfolio-recording-")) throw IllegalArgumentException()
                error = null; interrupted = false; recoveredElapsed = 0L
                require(!file.exists());allocated=file;path=file.path
                resetLevel(); startedAt = SystemClock.elapsedRealtime(); pausedAt = 0L; pausedMillis = 0L
                state = "recording"
                if (Build.VERSION.SDK_INT >= 30) startForeground(ID,notification(),ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE) else startForeground(ID,notification())
                file.parentFile?.mkdirs()
                require(file.name.removeSuffix(".partial").endsWith(".wav"))
                recorder = RecorderSession { handler.post { if (recorder != null) finish(true) } }
                recorder!!.start(file)
                persist(this)
                handler.post(sampleLevel)
                wake = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:recording").apply { setReferenceCounted(false); acquire(4 * 60 * 60 * 1000L) }
                handler.postDelayed(deadline,4 * 60 * 60 * 1000L)
                RecordingTileService.refresh(this)
                ready?.invoke(null); ready = null
            } catch (error: Exception) {
                recorder?.release(); recorder = null; allocated?.delete(); state = "error"
                RecordingService.error = if(error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart"; persist(this); releaseWake()
                ready?.invoke(if (error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart"); ready = null; stopSelf()
            }
            }
            PAUSE -> if (state == "recording") try { recorder?.pause(); pausedAt = SystemClock.elapsedRealtime(); state = "paused"; inputLevel = 0f; update() } catch (_: Exception) { finish(true) }
            RESUME -> if (state == "paused") try { recorder?.resume(); pausedMillis += SystemClock.elapsedRealtime() - pausedAt; pausedAt = 0L; state = "recording"; update() } catch (_: Exception) { finish(true) }
            STOP -> finish(true)
        }
        return START_NOT_STICKY
    }
    private fun update() { persist(this); RecordingTileService.refresh(this); getSystemService(NotificationManager::class.java).notify(ID,notification()) }
    fun finish(fromNotification: Boolean): String {
        val active = recorder ?: return "errors.recordingUnavailable"
        recorder = null
        resetLevel()
        var error = ""
        try {
            check(active.stop()); if (File(path).length() <= 44) throw IllegalStateException()
            if (quickRecording) { val source=File(path);val target=File(path.removeSuffix(".partial"));if(!source.renameTo(target))throw IllegalStateException();path=target.path }
            if (pausedAt == 0L) pausedAt=SystemClock.elapsedRealtime()
            state = "finished"
        }
        catch (_: Exception) {
            val recovered = WavRecovery.recover(File(path))
            if(recovered != null) {
                recoveredElapsed = recovered; startedAt = 0L; state = "finished"; interrupted = true; RecordingService.error = "errors.recordingInterrupted"
                if(quickRecording) { val target=File(path.removeSuffix(".partial"));if(File(path).renameTo(target)) path=target.path else {state="error";error="errors.recordingStop"} }
            } else { state = "error"; error = "errors.recordingEmpty" }
        }
        finally { active.release() }
        RecordingService.error = if(error.isNotEmpty()) error else RecordingService.error
        releaseWake(); handler.removeCallbacks(deadline); persist(this)
        stopForeground(STOP_FOREGROUND_REMOVE)
        if(fromNotification && error.isEmpty()) getSystemService(NotificationManager::class.java).notify(ID,notification(true))
        stopSelf()
        RecordingTileService.refresh(this)
        return error
    }
    fun discard() { resetLevel(); try { recorder?.stop() } catch (_: Exception) {} ; recorder?.release(); recorder = null; File(path).delete(); state = "cancelled"; error = null; releaseWake(); persist(this); stopForeground(STOP_FOREGROUND_REMOVE); stopSelf() }
    private fun releaseWake() { if(wake?.isHeld == true) wake?.release(); wake = null }
    override fun onDestroy() {
        resetLevel(); handler.removeCallbacks(deadline)
        if (recorder != null) { finish(true) }
        releaseWake()
        instance = null; RecordingTileService.refresh(this); super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
}
