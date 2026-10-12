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
import android.util.Log
import org.json.JSONArray

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
        @Volatile var stateRevision = 0L
        @Volatile var error: String? = null
        @Volatile var interrupted = false
        @Volatile var recoveredElapsed = 0L
        @Volatile private var recoveredElapsedMillis = 0L
        @Volatile private var savedEnvelope = RecorderEnvelope(emptyList(), 0, 0)
        @Volatile var inputLevel = 0f
        @Volatile var quickRecording = false
        @Volatile var path = ""
        @Volatile var startedAt = 0L
        @Volatile var pausedAt = 0L
        @Volatile var pausedMillis = 0L
        fun elapsedMillis() = instance?.recorder?.elapsedMillis ?: recoveredElapsedMillis
        fun elapsedSeconds() = elapsedMillis() / 1000L
        fun telemetry(): JSONObject {
            val envelope = instance?.recorder?.envelope() ?: savedEnvelope
            return JSONObject().put("elapsedMillis", if(instance?.recorder!=null) envelope.elapsedMillis else recoveredElapsedMillis).put("envelope", JSONArray(envelope.levels))
                .put("envelopeSequence", envelope.sequence).put("envelopeIntervalMs",125).put("stateRevision",stateRevision)
        }
        fun snapshot(context: Context): JSONObject = synchronized(instance ?: RecordingService::class.java) {
            restore(context)
            telemetry().put("state",state).put("path",path).put("elapsedSeconds",elapsedSeconds()).put("quick",quickRecording)
                .put("level",inputLevel).put("error",error).put("interrupted",interrupted)
        }
        var ready: ((String?) -> Unit)? = null
        private fun preferences(context: Context) = context.getSharedPreferences("hearfolio-recording", Context.MODE_PRIVATE)
        fun persist(context: Context) {
            val telemetry = telemetry()
            if(path.isNotEmpty() && !quickRecording) try {
                val atomic=AtomicFile(File(path+".state.json"));val stream=atomic.startWrite()
                try { stream.write(telemetry.put("state",state).put("path",path).put("error",error).put("quick",quickRecording).put("elapsedSeconds",elapsedSeconds()).put("updatedAt",System.currentTimeMillis()).toString().toByteArray());atomic.finishWrite(stream) }
                catch(error:Exception) {atomic.failWrite(stream)}
            } catch(_:Exception) {}
            preferences(context).edit().putString("state",state).putString("path",path).putBoolean("quick",quickRecording)
                .putLong("elapsed",elapsedSeconds()).putLong("elapsedMillis",elapsedMillis()).putLong("stateRevision",stateRevision).putString("telemetry",telemetry.toString()).putString("error",error).putBoolean("interrupted",interrupted).commit()
        }
        fun acknowledge(context: Context, requestedPath: String? = null) = synchronized(instance ?: RecordingService::class.java) {
            // Orphan metadata after process death may still say recording. Only
            // an actual live capture forbids acknowledgement/reset.
            check(RecorderControlPolicy.canResetMetadata(instance?.recorder!=null))
            check(requestedPath==null || requestedPath==path)
            state = "idle"; stateRevision++;path = ""; error = null; interrupted = false; recoveredElapsed = 0L; recoveredElapsedMillis = 0L; savedEnvelope = RecorderEnvelope(emptyList(),0,0); startedAt = 0L; persist(context)
        }
        fun restore(context: Context) {
            if(instance != null) return
            if(path.isNotEmpty()) { if(state=="finished" && !File(path).exists()) acknowledge(context);return }
            val saved=preferences(context)
            stateRevision=maxOf(stateRevision,saved.getLong("stateRevision",0))
            path=saved.getString("path", "") ?: ""
            state=saved.getString("state","idle") ?: "idle"
            quickRecording=saved.getBoolean("quick",false)
            error=saved.getString("error",null);interrupted=saved.getBoolean("interrupted",false)
            recoveredElapsed=saved.getLong("elapsed",0);startedAt=0L
            recoveredElapsedMillis=saved.getLong("elapsedMillis",recoveredElapsed*1000)
            try {
                val telemetry=JSONObject(saved.getString("telemetry","{}") ?: "{}")
                val levels=telemetry.optJSONArray("envelope") ?: JSONArray()
                val sequence=telemetry.optLong("envelopeSequence",0).coerceAtLeast(0)
                val first=(levels.length()-480).coerceAtLeast(0)
                savedEnvelope=RecorderEnvelope((first until levels.length()).map { levels.optDouble(it,0.0).coerceIn(0.0,1.0).toFloat() },sequence,recoveredElapsedMillis)
            } catch(_:Exception) { savedEnvelope=RecorderEnvelope(emptyList(),0,recoveredElapsedMillis) }
            if(path.isEmpty()) return
            val file=try { File(path).canonicalFile } catch(_: Exception) { acknowledge(context); return }
            if(!file.path.startsWith(context.dataDir.canonicalPath+File.separator) || !file.name.startsWith("hearfolio-recording-")) { acknowledge(context); return }
            if(!file.exists()) { acknowledge(context); return }
            if(state == "recording" || state == "paused" || state == "stopping") {
                stateRevision++
                interrupted=true
                val recovered=WavRecovery.recover(file)
                if(recovered != null) {
                    recoveredElapsed=recovered;recoveredElapsedMillis=recovered*1000;state="finished";error="errors.recordingInterrupted"
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
    private var lastPersist = 0L
    private var destroyed = false
    private var finishFromNotification = false
    private val deferredFinish = Runnable { finish(finishFromNotification) }
    private val deadline = Runnable { finish(true) }
    // Both interfaces consume the same envelope from the capture session.
    private val sampleLevel = object : Runnable {
        override fun run() {
            synchronized(this@RecordingService) {
                inputLevel = if (state == "recording") recorder?.level ?: 0f else 0f
                if (recorder != null && SystemClock.elapsedRealtime()-lastPersist>=2000) { persist(this@RecordingService); lastPersist=SystemClock.elapsedRealtime() }
                if (recorder != null) handler.postDelayed(this, 100)
            }
        }
    }
    private fun resetLevel() { handler.removeCallbacks(sampleLevel); inputLevel = 0f }
    private fun tr(ru: String, en: String) = if (Locale.getDefault().language == "ru") ru else en
    private fun action(action: String): PendingIntent = PendingIntent.getService(this, action.hashCode(), Intent(this, RecordingService::class.java).setAction(action).putExtra("path",path), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
    private fun notification(done: Boolean = false): Notification {
        val open = PendingIntent.getActivity(this, ID, mainAppIntent(this), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val b = NotificationCompat.Builder(this, CHANNEL).setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle("Hearfolio").setContentText(if(done && state=="error") tr("Запись прервана. Откройте Hearfolio.","Recording interrupted. Open Hearfolio.") else if(done && interrupted) tr("Запись прервана. Записанное аудио сохранено.","Recording interrupted. Captured audio was saved.") else if (done) tr("Аудио записано", "Audio recorded") else if(state=="stopping") tr("Сохранение записи…","Saving recording…") else if (state == "paused") tr("Запись на паузе", "Recording paused") else tr("Идёт запись", "Recording in progress"))
            .setContentIntent(open).setOnlyAlertOnce(true).setOngoing(!done).setAutoCancel(done)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC).setCategory(NotificationCompat.CATEGORY_TRANSPORT)
        if (!done && state!="stopping") {
            b.addAction(0, if (state == "paused") tr("Продолжить", "Resume") else tr("Пауза", "Pause"), action(if (state == "paused") RESUME else PAUSE))
            b.addAction(0, tr("Сохранить", "Save"), action(STOP))
        }
        return b.build()
    }
    @Synchronized override fun onCreate() {
        super.onCreate(); instance = this
        stateRevision=maxOf(stateRevision,preferences(this).getLong("stateRevision",0))
        if (Build.VERSION.SDK_INT >= 26) getSystemService(NotificationManager::class.java).createNotificationChannel(NotificationChannel(CHANNEL,tr("Запись аудио","Audio recording"),NotificationManager.IMPORTANCE_LOW).apply { lockscreenVisibility = Notification.VISIBILITY_PUBLIC })
    }
    @Suppress("DEPRECATION")
    @Synchronized override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
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
                error = null; interrupted = false; recoveredElapsed = 0L; recoveredElapsedMillis=0L; savedEnvelope=RecorderEnvelope(emptyList(),0,0)
                finishFromNotification=false
                require(!file.exists());allocated=file;path=file.path
                resetLevel(); startedAt = SystemClock.elapsedRealtime(); pausedAt = 0L; pausedMillis = 0L
                state = "recording"; stateRevision++
                if (Build.VERSION.SDK_INT >= 30) startForeground(ID,notification(),ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE) else startForeground(ID,notification())
                file.parentFile?.mkdirs()
                require(file.name.removeSuffix(".partial").endsWith(".wav"))
                lateinit var capture: RecorderSession
                capture = RecorderSession { failure -> handler.post { synchronized(this@RecordingService) { if (recorder === capture) { Log.e("HearfolioRecording","Capture interrupted",failure); interrupted=true; error="errors.recordingInterrupted"; finish(true) } } } }
                recorder = capture
                recorder!!.start(file)
                persist(this)
                handler.post(sampleLevel)
                wake = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:recording").apply { setReferenceCounted(false); acquire(4 * 60 * 60 * 1000L) }
                handler.postDelayed(deadline,4 * 60 * 60 * 1000L)
                RecordingTileService.refresh(this)
                ready?.invoke(null); ready = null
            } catch (error: Exception) {
                recorder?.release(); recorder = null; allocated?.delete(); state = "error"; stateRevision++
                RecordingService.error = if(error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart"; persist(this); releaseWake()
                ready?.invoke(if (error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart"); ready = null; stopSelf()
            }
            }
            PAUSE -> setPaused(true)
            RESUME -> setPaused(false)
            STOP -> finish(true)
        }
        return START_NOT_STICKY
    }
    @Synchronized fun setPaused(paused: Boolean, requestedPath: String = path): Boolean {
        val active=recorder ?: return false
        if(!RecorderControlPolicy.accepts(path,requestedPath,state)) return false
        if(paused) active.pause() else active.resume()
        val next=if(paused) "paused" else "recording"
        if(state!=next) { state=next; stateRevision++ };inputLevel=if(paused) 0f else active.level
        update(); return true
    }
    private fun update() {
        persist(this)
        // Notification/bridge UI errors must never be converted into a Save action.
        try { RecordingTileService.refresh(this);getSystemService(NotificationManager::class.java).notify(ID,notification()) } catch(error:Exception) { Log.w("HearfolioRecording","Notification update failed",error) }
    }
    @Synchronized fun finish(fromNotification: Boolean, requestedPath: String = path): String {
        if(requestedPath!=path) return "errors.recordingUnavailable"
        val active = recorder ?: return "errors.recordingUnavailable"
        finishFromNotification=finishFromNotification || fromNotification
        resetLevel()
        // Stop requests are bounded. A slow write/fsync keeps ownership until
        // the worker actually closes its WAV; never publish/repair it early.
        val stopped = if(state=="stopping" && !active.isClosed) false else active.stop()
        if(!active.isClosed) {
            if(state!="stopping") {state="stopping";stateRevision++;update()}
            if(SystemClock.elapsedRealtime()-lastPersist>=2000) {persist(this);lastPersist=SystemClock.elapsedRealtime()}
            handler.removeCallbacks(deferredFinish);handler.postDelayed(deferredFinish,100)
            return "errors.recordingStop"
        }
        handler.removeCallbacks(deferredFinish)
        var error = ""
        try {
            check(stopped); if (File(path).length() <= 44) throw IllegalStateException()
            if (quickRecording) { val source=File(path);val target=File(path.removeSuffix(".partial"));if(!source.renameTo(target))throw IllegalStateException();path=target.path }
            if (pausedAt == 0L) pausedAt=SystemClock.elapsedRealtime()
            state = "finished"
        }
        catch (_: Exception) {
            // Never repair a header while its worker may still be committing IO.
            val recovered = if(active.isClosed) WavRecovery.recover(File(path)) else null
            if(recovered != null) {
                recoveredElapsed = recovered; startedAt = 0L; state = "finished"; interrupted = true; RecordingService.error = "errors.recordingInterrupted"
                if(quickRecording) { val target=File(path.removeSuffix(".partial"));if(File(path).renameTo(target)) path=target.path else {state="error";error="errors.recordingStop"} }
            } else { state = "error"; error = if(active.isClosed) "errors.recordingEmpty" else "errors.recordingInterrupted" }
        }
        finally { savedEnvelope=active.envelope(); recoveredElapsedMillis=active.elapsedMillis; recorder=null;stateRevision++ }
        RecordingService.error = if(error.isNotEmpty()) error else RecordingService.error
        releaseWake(); handler.removeCallbacks(deadline); persist(this)
        stopForeground(STOP_FOREGROUND_REMOVE)
        if(finishFromNotification) try { getSystemService(NotificationManager::class.java).notify(ID,notification(true)) } catch(failure:Exception) { Log.w("HearfolioRecording","Terminal notification failed",failure) }
        stopSelf()
        if(destroyed) instance=null
        RecordingTileService.refresh(this)
        return error
    }
    @Synchronized fun discard(requestedPath: String = path): Boolean {
        if(requestedPath!=path) return false
        resetLevel();handler.removeCallbacks(deferredFinish); try { recorder?.stop() } catch (_: Exception) {} ; recorder?.release(); recorder = null; File(path).delete(); state = "cancelled";stateRevision++; error = null; releaseWake(); persist(this); stopForeground(STOP_FOREGROUND_REMOVE); stopSelf()
        return true
    }
    private fun releaseWake() { if(wake?.isHeld == true) wake?.release(); wake = null }
    @Synchronized override fun onDestroy() {
        destroyed=true
        resetLevel(); handler.removeCallbacks(deadline)
        if (recorder != null) { finish(true) }
        if(recorder==null) {releaseWake();instance=null}
        RecordingTileService.refresh(this); super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
}
