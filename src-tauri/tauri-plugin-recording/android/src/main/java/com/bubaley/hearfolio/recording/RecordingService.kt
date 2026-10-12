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
            publish()
            telemetry().put("state",state).put("path",path).put("elapsedSeconds",elapsedSeconds()).put("quick",quickRecording)
                .put("level",inputLevel).put("error",error).put("interrupted",interrupted)
        }
        private val controls = RecorderLifecycleQueue()
        private val journalWrites = RecorderLifecycleQueue()
        internal val starts = RecorderStartRegistry()
        @Volatile private var cachedSnapshot: String? = null
        fun execute(action: () -> Unit) { controls.submit(action) }
        fun busy() = starts.pendingPath.isNotEmpty() || state in listOf("recording","paused","stopping")
        private fun publish() { cachedSnapshot=telemetry().put("state",state).put("path",path).put("elapsedSeconds",elapsedSeconds()).put("quick",quickRecording).put("level",inputLevel).put("error",error).put("interrupted",interrupted).toString() }
        fun cachedStatus(): JSONObject? {
            val ticket=starts.pendingTicket
            if(ticket!=null) return JSONObject().put("state","starting").put("path",ticket.path).put("quick",ticket.quick).put("stateRevision",ticket.revision).put("elapsedMillis",0).put("elapsedSeconds",0).put("envelope",JSONArray()).put("envelopeSequence",0).put("envelopeIntervalMs",125)
            return cachedSnapshot?.let { JSONObject(it) }
        }
        private fun preferences(context: Context) = context.getSharedPreferences("hearfolio-recording", Context.MODE_PRIVATE)
        fun persist(context: Context, durable: Boolean = false) {
            publish()
            // Snapshot values on the control queue. Disk latency never blocks a
            // pause/stop or the Android main thread; writes retain their order.
            val saved=JSONObject(cachedSnapshot!!).put("updatedAt",System.currentTimeMillis())
            val target=path
            val quick=quickRecording
            val prefs=preferences(context)
            val write: () -> Unit = {
                var journalFailure:Exception?=null
                if(target.isNotEmpty() && !quick) try {
                    val atomic=AtomicFile(File(target+".state.json"));val stream=atomic.startWrite()
                    try {stream.write(saved.toString().toByteArray());atomic.finishWrite(stream)}
                    catch(failure:Exception) {atomic.failWrite(stream);throw failure}
                } catch(failure:Exception) {journalFailure=failure;Log.e("HearfolioRecording","Recording journal failed",failure)}
                try {
                    val written=prefs.edit().putString("state",saved.getString("state")).putString("path",target).putBoolean("quick",quick)
                        .putLong("elapsed",saved.getLong("elapsedSeconds")).putLong("elapsedMillis",saved.getLong("elapsedMillis"))
                        .putLong("stateRevision",saved.getLong("stateRevision")).putString("telemetry",saved.toString())
                        .putString("error",saved.optString("error").takeIf {it.isNotEmpty() && it!="null"}).putBoolean("interrupted",saved.optBoolean("interrupted")).commit()
                    if(!written) {
                        Log.e("HearfolioRecording","Recording preferences could not be committed")
                        if(durable) throw IllegalStateException("Recording identity could not be persisted")
                    }
                    if(durable && journalFailure!=null && !quick) throw requireNotNull(journalFailure)
                } catch(failure:Exception) {Log.e("HearfolioRecording","Recording preferences failed",failure);if(durable)throw failure}
            }
            if(durable) journalWrites.commit(write) else journalWrites.submit(write)
        }
        fun acknowledge(context: Context, requestedPath: String? = null) = synchronized(instance ?: RecordingService::class.java) {
            // Orphan metadata after process death may still say recording. Only
            // an actual live capture forbids acknowledgement/reset.
            check(RecorderControlPolicy.canResetMetadata(instance?.recorder!=null))
            check(requestedPath==null || requestedPath==path)
            state = "idle"; stateRevision++;path = ""; error = null; interrupted = false; recoveredElapsed = 0L; recoveredElapsedMillis = 0L; savedEnvelope = RecorderEnvelope(emptyList(),0,0); startedAt = 0L; persist(context)
        }
        fun restore(context: Context) {
            if(instance?.recorder != null) return
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
            if(RecorderControlPolicy.shouldRecoverOrphan(state,instance?.recorder!=null)) {
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
    @Volatile private var recorder: RecorderSession? = null
    @Volatile private var latestStartId = 0
    private var captureStartId = 0
    @Volatile private var latestRecordingStartId = 0
    private var wake: PowerManager.WakeLock? = null
    private val handler = Handler(Looper.getMainLooper())
    private var lastPersist = 0L
    @Volatile private var destroyed = false
    private var finishFromNotification = false
    private var cancellationRequested = false
    private val deferredFinish = Runnable { execute { if(instance===this) finish(finishFromNotification) } }
    private val deadline = Runnable { execute { if(instance===this) finish(true) } }
    // Both interfaces consume the same envelope from the capture session.
    private val sampleLevel = object : Runnable {
        override fun run() {
            execute {
                if(instance!==this@RecordingService) return@execute
                inputLevel = if (state == "recording") recorder?.level ?: 0f else 0f
                if (recorder != null && SystemClock.elapsedRealtime()-lastPersist>=2000) { persist(this@RecordingService); lastPersist=SystemClock.elapsedRealtime() }
                publish()
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
    override fun onCreate() {
        super.onCreate()
        execute {
            // Restore notification-launched orphan metadata before attaching an
            // empty service. Never replace a still-closing capture owner.
            if(instance?.recorder==null) {restore(this);instance=this;publish()}
        }
        if (Build.VERSION.SDK_INT >= 26) getSystemService(NotificationManager::class.java).createNotificationChannel(NotificationChannel(CHANNEL,tr("Запись аудио","Audio recording"),NotificationManager.IMPORTANCE_LOW).apply { lockscreenVisibility = Notification.VISIBILITY_PUBLIC })
    }
    @Suppress("DEPRECATION")
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        latestStartId=startId
        if(intent==null) { stopSelf(startId);return START_NOT_STICKY }
        val intentPath=intent.getStringExtra("path") ?: ""
        val pending=if(intent.action==START) starts.pendingTicket else null
        val ticket=if(intent.action==START) starts.claim(intentPath) else null
        if(intent.action==START) {
            val reason=when {ticket!=null -> "claimed";pending==null -> "no_pending_ticket";pending.cancelled -> "cancelled_ticket";pending.path!=intentPath -> "path_mismatch";else -> "ticket_changed"}
            Log.i("HearfolioRecording","Service START: startId=$startId; intentPath=$intentPath; pendingPath=${pending?.path}; claim=$reason")
            // Even a cancelled queued START came from startForegroundService.
            // Fulfil that contract before rejecting it; otherwise Android may
            // crash the process for a foreground service that never promoted.
            if(ticket!=null) latestRecordingStartId=startId
            try {
                val accepted=RecorderForegroundStart.accept(ticket!=null,
                    promote={if(Build.VERSION.SDK_INT>=30) startForeground(ID,notification(),ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE) else startForeground(ID,notification())},
                    reject={if(recorder==null)stopSelf(startId)})
                if(!accepted) return START_NOT_STICKY
            } catch(failure:Exception) {
                Log.e("HearfolioRecording","Foreground promotion failed",failure)
                ticket?.let {starts.complete(it,if(failure is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart")}
                stopSelf(startId);return START_NOT_STICKY
            }
        }
        execute { handleCommand(intent,startId,ticket) }
        return START_NOT_STICKY
    }
    private fun handleCommand(intent: Intent, startId: Int, ticket: RecorderStartRegistry.Ticket?) {
        if(instance!==this || destroyed) { ticket?.let { starts.complete(it,"errors.recordingUnavailable") };stopSelf(startId);return }
        if(intent.action!=START && intent.getStringExtra("path")!=path) {if(recorder==null)stopSelf(startId);return}
        when (intent.action) {
            START -> {
                if(ticket==null || ticket.cancelled) { if(recorder==null) stopSelf(startId);return }
                if (recorder != null) { starts.complete(ticket,"errors.operationBusy");return }
                captureStartId=startId
                ticket.revision=maxOf(ticket.revision,stateRevision+1)
                var allocated: File? = null
                try {
                quickRecording = intent.getBooleanExtra("quick",false)
                val file = File(intent.getStringExtra("path") ?: throw IllegalArgumentException()).canonicalFile
                if (!file.path.startsWith(dataDir.canonicalPath + File.separator) || !file.name.startsWith("hearfolio-recording-")) throw IllegalArgumentException()
                error = null; interrupted = false; recoveredElapsed = 0L; recoveredElapsedMillis=0L; savedEnvelope=RecorderEnvelope(emptyList(),0,0)
                finishFromNotification=false;cancellationRequested=false
                require(!file.exists());allocated=file;path=file.path
                resetLevel(); startedAt = SystemClock.elapsedRealtime(); pausedAt = 0L; pausedMillis = 0L
                state = "starting";stateRevision=ticket.revision
                persist(this,durable=true)
                if(ticket.cancelled) {state="cancelled";stateRevision++;persist(this);stopSelf(startId);return}
                if (Build.VERSION.SDK_INT >= 30) startForeground(ID,notification(),ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE) else startForeground(ID,notification())
                file.parentFile?.mkdirs()
                require(file.name.removeSuffix(".partial").endsWith(".wav"))
                lateinit var capture: RecorderSession
                capture = RecorderSession { failure -> execute { if (instance===this && recorder === capture) { Log.e("HearfolioRecording","Capture interrupted",failure); interrupted=true; error="errors.recordingInterrupted"; finish(true) } } }
                recorder = capture
                recorder!!.start(file)
                state="recording";stateRevision=ticket.revision+1
                persist(this)
                handler.post(sampleLevel)
                wake = (getSystemService(POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"Hearfolio:recording").apply { setReferenceCounted(false); acquire(4 * 60 * 60 * 1000L) }
                handler.postDelayed(deadline,4 * 60 * 60 * 1000L)
                RecordingTileService.refresh(this)
                starts.complete(ticket,null)
            } catch (error: Exception) {
                Log.e("HearfolioRecording","Recording startup failed",error)
                val active=recorder
                if(active!=null && allocated?.length()?.let {it>44}==true) {
                    interrupted=true;RecordingService.error="errors.recordingInterrupted"
                    finish(true)
                    starts.complete(ticket,"errors.recordingInterrupted")
                    return
                }
                active?.stop()
                if(active!=null && !active.isClosed) {
                    interrupted=true;RecordingService.error="errors.recordingInterrupted";state="stopping";stateRevision++;persist(this)
                    finishFromNotification=true;handler.postDelayed(deferredFinish,100)
                    starts.complete(ticket,"errors.recordingInterrupted")
                    return
                }
                recorder = null; allocated?.delete(); state = "error"; stateRevision++
                RecordingService.error = if(error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart"; persist(this); releaseWake()
                starts.complete(ticket,RecordingService.error); stopSelf(startId)
            }
            }
            PAUSE -> setPaused(true)
            RESUME -> setPaused(false)
            STOP -> { if(recorder==null) {stopSelf(startId)} else finish(true) }
        }
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
        if(state!="stopping") {state="stopping";stateRevision++;update()}
        val stopped = active.stop()
        if(!active.isClosed) {
            if(SystemClock.elapsedRealtime()-lastPersist>=2000) {persist(this);lastPersist=SystemClock.elapsedRealtime()}
            handler.removeCallbacks(deferredFinish);handler.postDelayed(deferredFinish,100)
            return "errors.recordingStop"
        }
        handler.removeCallbacks(deferredFinish)
        var error = ""
        try {
            if(cancellationRequested) {File(path).delete();state="cancelled"}
            else {
            check(stopped); if (File(path).length() <= 44) throw IllegalStateException()
            if (quickRecording) { val source=File(path);val target=File(path.removeSuffix(".partial"));if(!source.renameTo(target))throw IllegalStateException();path=target.path }
            if (pausedAt == 0L) pausedAt=SystemClock.elapsedRealtime()
            state = "finished"
            }
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
        if(latestRecordingStartId==captureStartId) stopForeground(STOP_FOREGROUND_REMOVE)
        if(finishFromNotification && !cancellationRequested) try { getSystemService(NotificationManager::class.java).notify(ID,notification(true)) } catch(failure:Exception) { Log.w("HearfolioRecording","Terminal notification failed",failure) }
        stopSelf(if(latestRecordingStartId==captureStartId) latestStartId else captureStartId)
        if(destroyed && instance===this) instance=null
        RecordingTileService.refresh(this)
        return error
    }
    @Synchronized fun discard(requestedPath: String = path): Boolean {
        if(requestedPath!=path) return false
        if(recorder!=null) {cancellationRequested=true;finish(false,requestedPath);return state=="cancelled"}
        File(path).delete();state="cancelled";stateRevision++;error=null;persist(this)
        stopSelf(if(latestRecordingStartId==captureStartId) latestStartId else captureStartId);return true
    }
    private fun releaseWake() { if(wake?.isHeld == true) wake?.release(); wake = null }
    override fun onDestroy() {
        destroyed=true
        resetLevel(); handler.removeCallbacks(deadline)
        execute {
            if(instance===this) {
                if (recorder != null) finish(true)
                if(recorder==null) {releaseWake();instance=null}
                RecordingTileService.refresh(this)
            }
        }
        super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
}
