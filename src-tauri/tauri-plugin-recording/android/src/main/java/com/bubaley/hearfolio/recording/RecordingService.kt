package com.bubaley.hearfolio.recording

import android.app.*
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.MediaRecorder
import android.os.*
import androidx.core.app.NotificationCompat
import java.io.File
import java.util.Locale

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
        @Volatile var inputLevel = 0f
        @Volatile var quickRecording = false
        @Volatile var path = ""
        @Volatile var startedAt = 0L
        @Volatile var pausedAt = 0L
        @Volatile var pausedMillis = 0L
        fun elapsedSeconds() = if (startedAt == 0L) 0L else ((if (pausedAt > 0L) pausedAt else SystemClock.elapsedRealtime()) - startedAt - pausedMillis).coerceAtLeast(0L) / 1000L
        var ready: ((String?) -> Unit)? = null
    }
    private var recorder: MediaRecorder? = null
    private var wake: PowerManager.WakeLock? = null
    private val handler = Handler(Looper.getMainLooper())
    private val deadline = Runnable { finish(true) }
    // getMaxAmplitude resets the peak each time: sample once in the service so
    // the WebView and lock-screen meter share the same microphone envelope.
    private val sampleLevel = object : Runnable {
        override fun run() {
            inputLevel = if (state == "recording") try {
                val peak = (recorder?.maxAmplitude ?: 0) / 32767.0
                if (peak <= .001) 0f else ((20 * kotlin.math.log10(peak) + 60) / 60).coerceIn(0.0, 1.0).toFloat()
            } catch (_: Exception) { 0f } else 0f
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
            START -> try {
                if (recorder != null) throw IllegalStateException()
                quickRecording = intent.getBooleanExtra("quick",false)
                path = intent.getStringExtra("path") ?: throw IllegalArgumentException()
                val file = File(path).canonicalFile
                if (!file.path.startsWith(dataDir.canonicalPath + File.separator) || !file.name.startsWith("hearfolio-recording-")) throw IllegalArgumentException()
                resetLevel(); startedAt = SystemClock.elapsedRealtime(); pausedAt = 0L; pausedMillis = 0L
                state = "recording"
                if (Build.VERSION.SDK_INT >= 30) startForeground(ID,notification(),ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE) else startForeground(ID,notification())
                file.parentFile?.mkdirs()
                recorder = if (Build.VERSION.SDK_INT >= 31) MediaRecorder(this) else MediaRecorder()
                recorder!!.apply {
                    setAudioSource(MediaRecorder.AudioSource.MIC); setOutputFormat(MediaRecorder.OutputFormat.MPEG_4); setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
                    setAudioChannels(1); setAudioSamplingRate(44100); setAudioEncodingBitRate(96000); setOutputFile(path); prepare(); start()
                }
                handler.post(sampleLevel)
                wake = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:recording").apply { setReferenceCounted(false); acquire(4 * 60 * 60 * 1000L) }
                handler.postDelayed(deadline,4 * 60 * 60 * 1000L)
                RecordingTileService.refresh(this)
                ready?.invoke(null); ready = null
            } catch (error: Exception) {
                recorder?.release(); recorder = null; File(path).delete(); state = "error"
                ready?.invoke(if (error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart"); ready = null; stopSelf()
            }
            PAUSE -> if (state == "recording") try { recorder?.pause(); pausedAt = SystemClock.elapsedRealtime(); state = "paused"; inputLevel = 0f; update() } catch (_: Exception) { finish(true) }
            RESUME -> if (state == "paused") try { recorder?.resume(); pausedMillis += SystemClock.elapsedRealtime() - pausedAt; pausedAt = 0L; state = "recording"; update() } catch (_: Exception) { finish(true) }
            STOP -> finish(true)
        }
        return START_NOT_STICKY
    }
    private fun update() { RecordingTileService.refresh(this); getSystemService(NotificationManager::class.java).notify(ID,notification()) }
    fun finish(fromNotification: Boolean): String {
        val active = recorder ?: return "errors.recordingUnavailable"
        recorder = null
        resetLevel()
        var error = ""
        try {
            active.stop(); if (File(path).length() <= 44) throw IllegalStateException()
            if (quickRecording) { val source=File(path);val target=File(path.removeSuffix(".partial"));if(!source.renameTo(target))throw IllegalStateException();path=target.path }
            if (pausedAt == 0L) pausedAt=SystemClock.elapsedRealtime()
            state = if (fromNotification || quickRecording) "finished" else "idle"
        }
        catch (_: Exception) { File(path).delete(); state = "error"; error = "errors.recordingEmpty" }
        finally { active.release() }
        // Keep the service alive until Rust atomically archives the completed file.
        if (fromNotification && error.isEmpty()) getSystemService(NotificationManager::class.java).notify(ID,notification(true))
        else { stopForeground(STOP_FOREGROUND_REMOVE); stopSelf() }
        if (quickRecording) {stopForeground(STOP_FOREGROUND_REMOVE);stopSelf()}
        RecordingTileService.refresh(this)
        return error
    }
    fun discard() { resetLevel(); try { recorder?.stop() } catch (_: Exception) {} ; recorder?.release(); recorder = null; File(path).delete(); state = "cancelled"; stopForeground(STOP_FOREGROUND_REMOVE); stopSelf() }
    override fun onDestroy() {
        resetLevel(); handler.removeCallbacks(deadline)
        if (recorder != null) { finish(true) }
        if (wake?.isHeld == true) wake?.release()
        instance = null; RecordingTileService.refresh(this); super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
}
