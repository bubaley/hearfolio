package com.bubaley.hearfolio.recording

import android.Manifest
import android.app.Activity
import android.content.ClipData
import androidx.core.content.ContextCompat
import android.content.Intent
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaPlayer
import android.media.MediaRecorder
import android.media.audiofx.AudioEffect
import android.media.audiofx.AutomaticGainControl
import android.util.Log
import android.os.Build
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.FileProvider
import app.tauri.plugin.JSObject
import app.tauri.PermissionState
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.io.File
import java.io.RandomAccessFile
import java.util.UUID
import kotlin.math.sqrt

@InvokeArg
class StartArgs { lateinit var path: String }
@InvokeArg
class RecordingControlArgs { var stage: String = ""; var path: String = "" }

@InvokeArg
class AudioArgs { lateinit var path: String; lateinit var name: String }
@InvokeArg
class TextArgs { lateinit var name: String; lateinit var text: String }
@InvokeArg
class SeekArgs { var seconds: Double = 0.0 }

@InvokeArg
class OperationArgs {
    lateinit var id: String
    var snapshotPath: String = ""
    var controlPath: String = ""
    var kind: String = "transcribe"
    var network: Boolean = false
    var stage: String = ""
    var current: Long = 0
    var total: Long = 0
    var success: Boolean = false
}
@InvokeArg
class TransferArgs {
    var path: String = ""
    var snapshotPath: String = ""
    var controlPath: String = ""
    lateinit var id: String
    var direction: String = "send"
    var stage: String = "waiting"
    var current: Long = 0
    var total: Long = 0
    var success: Boolean = false
}

@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone"), Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications")])
class RecordingPlugin(private val activity: Activity) : Plugin(activity) {
    private var player: MediaPlayer? = null
    private var playbackGeneration = 0
    private var preparingPlayback: Invoke? = null
    private var pending: Invoke? = null

    @Command
    fun start(invoke: Invoke) {
        if (RecordingService.instance != null || pending != null) { invoke.reject("errors.operationBusy"); return }
        releasePlayer()
        pending = invoke
        if (getPermissionState("microphone") != PermissionState.GRANTED) {
            requestPermissionForAlias("microphone", invoke, "microphonePermissionResult")
        } else { startWithNotifications(invoke) }
    }

    @PermissionCallback
    fun microphonePermissionResult(invoke: Invoke) {
        if (pending !== invoke) { return }
        if (getPermissionState("microphone") != PermissionState.GRANTED) {
            pending = null
            invoke.reject("errors.microphonePermissionDenied")
        } else { startWithNotifications(invoke) }
    }

    private fun startWithNotifications(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= 33 && getPermissionState("notifications") != PermissionState.GRANTED) requestPermissionForAlias("notifications", invoke, "recordingNotificationResult") else startGranted(invoke)
    }
    @PermissionCallback
    fun recordingNotificationResult(invoke: Invoke) { if(pending===invoke) startGranted(invoke) }
    @Command
    fun acknowledgeRecording(invoke: Invoke) {
        val requestedPath=invoke.parseArgs(StartArgs::class.java).path
        if(requestedPath!=RecordingService.path) {invoke.reject("errors.recordingUnavailable");return}
        if(!RecorderControlPolicy.canAcknowledge(RecordingService.state)) { invoke.reject("errors.operationBusy"); return }
        try {RecordingService.acknowledge(activity.applicationContext,requestedPath);activity.stopService(Intent(activity, RecordingService::class.java));invoke.resolve()}
        catch(_:Exception) {invoke.reject("errors.recordingUnavailable")}
    }

    private fun startGranted(invoke: Invoke) {
        try {
            val file = File(invoke.parseArgs(StartArgs::class.java).path).canonicalFile
            if (!file.path.startsWith(activity.dataDir.canonicalPath + File.separator) || !file.name.startsWith("hearfolio-recording-")) throw IllegalArgumentException()
            RecordingService.ready = { error -> pending = null; if (error == null) invoke.resolve() else invoke.reject(error) }
            ContextCompat.startForegroundService(activity, Intent(activity, RecordingService::class.java).setAction(RecordingService.START).putExtra("path", file.path))
        } catch (_: Exception) { RecordingService.ready = null; pending = null; invoke.reject("errors.recordingStart") }
    }

    @Command
    fun stop(invoke: Invoke) {
        val requestedPath=invoke.parseArgs(StartArgs::class.java).path
        if(requestedPath!=RecordingService.path) {invoke.reject("errors.recordingUnavailable");return}
        if (RecordingService.state == "finished") { RecordingService.acknowledge(activity.applicationContext,requestedPath); activity.stopService(Intent(activity, RecordingService::class.java)); invoke.resolve(); return }
        val error = RecordingService.instance?.finish(false,requestedPath) ?: "errors.recordingUnavailable"
        if (error.isEmpty()) { RecordingService.acknowledge(activity.applicationContext,requestedPath); invoke.resolve() } else invoke.reject(error)
    }
    @Command
    fun cancel(invoke: Invoke) {
        val requestedPath=invoke.parseArgs(StartArgs::class.java).path
        if(pending?.parseArgs(StartArgs::class.java)?.path==requestedPath) {pending?.reject("errors.recordingUnavailable");pending=null;RecordingService.ready=null}
        val owner=RecordingService.instance
        if(owner==null || owner.discard(requestedPath)) invoke.resolve() else invoke.reject("errors.recordingUnavailable")
    }
    @Command
    fun recordingLevel(invoke: Invoke) { invoke.resolve(JSObject().put("level", RecordingService.inputLevel)) }
    @Command
    fun recordingStatus(invoke: Invoke) {
        val snapshot=RecordingService.snapshot(activity.applicationContext)
        val result = JSObject();snapshot.keys().forEach { key -> result.put(key,snapshot.get(key)) };invoke.resolve(result)
    }
    @Command
    fun recordingControl(invoke: Invoke) {
        val args=invoke.parseArgs(RecordingControlArgs::class.java)
        val stage = args.stage
        val paused = when(stage) { "pause" -> true; "resume" -> false; else -> { invoke.reject("errors.recordingUnavailable"); return } }
        if(RecordingService.instance?.setPaused(paused,args.path)==true) invoke.resolve() else invoke.reject("errors.recordingUnavailable")
    }
    @Command
    fun openBatterySettings(invoke: Invoke) {
        try {
            activity.startActivity(Intent(android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS, android.net.Uri.parse("package:"+activity.packageName)))
            invoke.resolve()
        } catch(_: Exception) { invoke.reject("errors.operationFailed") }
    }

    @Command
    fun quickControl(invoke: Invoke) {
        if (!RecordingService.quickRecording) { invoke.reject("errors.recordingUnavailable"); return }
        val args=invoke.parseArgs(TransferArgs::class.java)
        val action=args.stage
        if(args.path!=RecordingService.path) {invoke.reject("errors.recordingUnavailable");return}
        when(action) {
            "pause", "resume" -> { if(RecordingService.instance?.setPaused(action=="pause",args.path)==true) invoke.resolve() else invoke.reject("errors.recordingUnavailable") }
            "stop" -> { if (RecordingService.state == "finished") invoke.resolve() else { val error=RecordingService.instance?.finish(true,args.path) ?: "errors.recordingUnavailable"; if(error.isEmpty())invoke.resolve() else invoke.reject(error) } }
            "cancel" -> {if(RecordingService.instance?.discard(args.path)!=false)invoke.resolve() else invoke.reject("errors.recordingUnavailable")}
            else -> invoke.reject("errors.recordingUnavailable")
        }
    }

    @Command
    fun quickRecordings(invoke: Invoke) {
        val dir=File(activity.filesDir,"hearfolio-quick-recordings")
        // Keep interrupted quick recordings even if another quick session has
        // since replaced the single last-session preference.
        dir.listFiles()?.filter { it.isFile && it.canonicalFile.parentFile==dir.canonicalFile && it.name.startsWith("hearfolio-recording-") && it.name.endsWith(".wav.partial") && (RecordingService.instance==null || it.canonicalPath!=RecordingService.path) }?.forEach { source ->
            if(WavRecovery.recover(source)!=null) {val target=File(source.path.removeSuffix(".partial"));if(!target.exists())source.renameTo(target)}
        }
        val files=dir.listFiles()?.filter { it.isFile && it.canonicalFile.parentFile==dir.canonicalFile && it.name.startsWith("hearfolio-recording-") && (it.name.endsWith(".m4a") || it.name.endsWith(".wav")) }?.map { it.canonicalPath } ?: emptyList()
        val result=JSObject();result.put("paths",org.json.JSONArray(files));invoke.resolve(result)
    }

    @Command
    fun startInbox(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= 33 && getPermissionState("notifications") != PermissionState.GRANTED) {
            requestPermissionForAlias("notifications", invoke, "inboxPermissionResult")
        } else startInboxGranted(invoke)
    }
    @PermissionCallback
    fun inboxPermissionResult(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= 33 && getPermissionState("notifications") != PermissionState.GRANTED) invoke.reject("errors.transferBackgroundUnavailable") else startInboxGranted(invoke)
    }
    private fun startInboxGranted(invoke: Invoke) {
        try {
            InboxService.ready = { error -> if (error == null) invoke.resolve() else invoke.reject(error) }
            val args=invoke.parseArgs(TransferArgs::class.java)
            ContextCompat.startForegroundService(activity, Intent(activity, InboxService::class.java).setAction(InboxService.START).putExtra("snapshotPath",args.snapshotPath).putExtra("controlPath",args.controlPath))
        } catch (_: Exception) { InboxService.ready = null; invoke.reject("errors.transferBackgroundUnavailable") }
    }
    @Command
    fun stopInbox(invoke: Invoke) { activity.stopService(Intent(activity, InboxService::class.java)); invoke.resolve() }
    @Command
    fun inboxStatus(invoke: Invoke) {
        val result = JSObject(); result.put("active", InboxService.active);result.put("error",InboxService.failure);invoke.resolve(result)
    }
    @Command
    fun inboxOffers(invoke: Invoke) {
        val args = invoke.parseArgs(TransferArgs::class.java)
        InboxService.instance?.offers(args.current.toInt(), args.success)
        invoke.resolve()
    }

    @Command
    fun beginOperation(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(OperationArgs::class.java)
            OperationService.ready = { error -> if(error == null) invoke.resolve() else invoke.reject(error) }
            ContextCompat.startForegroundService(activity.applicationContext, Intent(activity.applicationContext, OperationService::class.java)
                .setAction(OperationService.BEGIN).putExtra("id",args.id).putExtra("kind",args.kind).putExtra("network",args.network).putExtra("snapshotPath",args.snapshotPath).putExtra("controlPath",args.controlPath))
        } catch(_: Exception) { OperationService.ready = null; invoke.reject("errors.operationBackgroundUnavailable") }
    }
    @Command
    fun checkOperation(invoke: Invoke) {
        val args = invoke.parseArgs(OperationArgs::class.java)
        invoke.resolve(JSObject().put("cancelled",OperationService.isCancelled(args.id)).put("error",OperationService.error))
    }
    @Command
    fun updateOperation(invoke: Invoke) {
        val args = invoke.parseArgs(OperationArgs::class.java)
        if(OperationService.job == args.id) OperationService.instance?.update(args.stage,args.current,args.total)
        invoke.resolve()
    }
    @Command
    fun finishOperation(invoke: Invoke) {
        val args = invoke.parseArgs(OperationArgs::class.java)
        if(OperationService.job == args.id) OperationService.instance?.finish(args.success)
        invoke.resolve()
    }
    @Command
    fun cancelOperation(invoke: Invoke) {
        val args = invoke.parseArgs(OperationArgs::class.java)
        if(OperationService.job == args.id) {
            OperationService.cancelled = true; OperationService.error = "errors.operationCancelled"
            OperationService.instance?.update("cancelled",0,0)
        }
        invoke.resolve()
    }

    @Command
    fun beginTransfer(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= 33 && getPermissionState("notifications") != PermissionState.GRANTED) {
            requestPermissionForAlias("notifications", invoke, "transferNotificationResult")
        } else { startTransfer(invoke) }
    }

    @PermissionCallback
    fun transferNotificationResult(invoke: Invoke) { startTransfer(invoke) }

    private fun startTransfer(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(TransferArgs::class.java)
            TransferService.prepare(args.id)
            TransferService.ready = { error -> if (error == null) invoke.resolve() else invoke.reject(error) }
            ContextCompat.startForegroundService(activity, Intent(activity, TransferService::class.java).setAction(TransferService.BEGIN).putExtra("id",args.id).putExtra("direction",args.direction).putExtra("snapshotPath",args.snapshotPath).putExtra("controlPath",args.controlPath))
        } catch (error: Exception) { TransferService.ready = null; invoke.reject("errors.transferBackgroundUnavailable") }
    }

    @Command
    fun checkTransfer(invoke: Invoke) {
        val args = invoke.parseArgs(TransferArgs::class.java)
        val response = JSObject(); response.put("cancelled", TransferService.isCancelled(args.id)); invoke.resolve(response)
    }

    @Command
    fun updateTransfer(invoke: Invoke) {
        val args = invoke.parseArgs(TransferArgs::class.java)
        if (TransferService.job == args.id) { if (InboxService.active) InboxService.instance?.progress(args.stage,args.current,args.total) else TransferService.instance?.update(args.stage,args.current,args.total) }
        invoke.resolve()
    }

    @Command
    fun finishTransfer(invoke: Invoke) {
        val args = invoke.parseArgs(TransferArgs::class.java)
        if (InboxService.active && TransferService.job == args.id) { InboxService.instance?.transfer(false, args.success); invoke.resolve(); return }
        if (TransferService.job == args.id && TransferService.instance != null) {
            TransferService.stopped = { invoke.resolve() }
            activity.startService(Intent(activity, TransferService::class.java).setAction(TransferService.END).putExtra("id",args.id).putExtra("success",args.success))
        } else { invoke.resolve() }
    }

    override fun onDestroy(activity: AppCompatActivity) { releasePlayer(); /* Services own recording and transfer lifetimes. */ }
    private fun privateFile(path: String): File {
        val file = File(path).canonicalFile
        require(file.path.startsWith(activity.dataDir.canonicalPath + File.separator))
        require(file.isFile)
        return file
    }

    private fun releasePlayer() {
        playbackGeneration++
        preparingPlayback?.reject("errors.audioPlayback"); preparingPlayback = null
        player?.release(); player = null
    }
    private fun playerState(): JSObject {
        val active = player ?: throw IllegalStateException("No playback")
        return JSObject().put("duration", active.duration / 1000.0)
            .put("currentTime", active.currentPosition / 1000.0).put("playing", active.isPlaying)
    }
    @Command
    fun preparePlayback(invoke: Invoke) {
        if (RecordingService.instance != null || pending != null) { invoke.reject("errors.operationBusy"); return }
        releasePlayer()
        try {
            val file = privateFile(invoke.parseArgs(StartArgs::class.java).path)
            val next = MediaPlayer()
            player = next
            val generation = playbackGeneration
            preparingPlayback = invoke
            next.setDataSource(file.path)
            next.setOnErrorListener { _, _, _ ->
                if (generation == playbackGeneration && player === next) releasePlayer()
                true
            }
            next.setOnPreparedListener {
                if (generation == playbackGeneration && player === next) {
                    preparingPlayback = null
                    try { invoke.resolve(playerState()) } catch (_: Exception) { invoke.reject("errors.audioPlayback") }
                }
            }
            next.prepareAsync()
        } catch (_: Exception) { preparingPlayback = null; releasePlayer(); invoke.reject("errors.audioPlayback") }
    }
    @Command
    fun playbackState(invoke: Invoke) {
        try { invoke.resolve(playerState()) } catch (_: Exception) { invoke.reject("errors.audioPlayback") }
    }
    @Command
    fun playPlayback(invoke: Invoke) {
        try { requireNotNull(player).start(); invoke.resolve() } catch (_: Exception) { invoke.reject("errors.audioPlayback") }
    }
    @Command
    fun pausePlayback(invoke: Invoke) {
        try { requireNotNull(player).pause(); invoke.resolve() } catch (_: Exception) { invoke.reject("errors.audioPlayback") }
    }
    @Command
    fun seekPlayback(invoke: Invoke) {
        try {
            val active = requireNotNull(player)
            val seconds = invoke.parseArgs(SeekArgs::class.java).seconds
            require(seconds.isFinite())
            val target = (seconds * 1000).coerceIn(0.0, active.duration.toDouble()).toInt()
            if (Build.VERSION.SDK_INT >= 26) active.seekTo(target.toLong(), MediaPlayer.SEEK_CLOSEST) else active.seekTo(target)
            invoke.resolve()
        } catch (_: Exception) { invoke.reject("errors.audioPlayback") }
    }
    @Command
    fun releasePlayback(invoke: Invoke) { releasePlayer(); invoke.resolve() }
    @Command
    fun audioDuration(invoke: Invoke) {
        var metadata: android.media.MediaMetadataRetriever? = null
        try {
            val file = privateFile(invoke.parseArgs(StartArgs::class.java).path)
            metadata = android.media.MediaMetadataRetriever()
            metadata.setDataSource(file.path)
            val duration = metadata.extractMetadata(android.media.MediaMetadataRetriever.METADATA_KEY_DURATION)?.toDoubleOrNull()
            invoke.resolve(JSObject().put("duration", duration?.div(1000.0)))
        } catch (_: Exception) { invoke.resolve(JSObject().put("duration", null)) }
        finally { metadata?.release() }
    }

    private fun exportName(name: String, extension: String): String {
        val base = name.substringAfterLast('/').substringAfterLast('\\').replace(Regex("[\\x00-\\x1f\\x7f]"), "").take(180).ifBlank { "Recording" }
        return if (base.endsWith(".$extension", true)) base else "$base.$extension"
    }
    private fun mime(file: File) = when (file.extension.lowercase()) {
        "wav" -> "audio/wav"; "m4a", "mp4" -> "audio/mp4"; "mp3" -> "audio/mpeg"
        "aac" -> "audio/aac"; "flac" -> "audio/flac"; "ogg" -> "audio/ogg"; else -> "application/octet-stream"
    }

    @Command
    fun shareAudio(invoke: Invoke) {
        Thread({
            try {
                val args = invoke.parseArgs(AudioArgs::class.java)
                val source = privateFile(args.path)
                val directory = File(activity.cacheDir, "hearfolio-shares").also { it.mkdirs() }
                // Retain in-flight shares; expire only old copies on the next share.
                directory.listFiles()?.filter { it.lastModified() < System.currentTimeMillis() - 24 * 60 * 60 * 1000 }?.forEach { it.deleteRecursively() }
                val sharedDirectory = File(directory, UUID.randomUUID().toString()).also { it.mkdir() }
                val shared = File(sharedDirectory, exportName(args.name, source.extension)).also { source.copyTo(it) }
                val uri = FileProvider.getUriForFile(activity, activity.packageName + ".hearfolio.files", shared)
                val intent = Intent(Intent.ACTION_SEND).setType(mime(source)).putExtra(Intent.EXTRA_STREAM, uri)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                intent.clipData = ClipData.newUri(activity.contentResolver, args.name, uri)
                activity.runOnUiThread {
                    try { activity.startActivity(Intent.createChooser(intent, null)); invoke.resolve() }
                    catch (_: Exception) { invoke.reject("errors.audioShare") }
                }
            } catch (_: Exception) { invoke.reject("errors.audioShare") }
        }, "HearfolioAudioShare").start()
    }

    @Command
    fun shareText(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(TextArgs::class.java)
            activity.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain")
                .putExtra(Intent.EXTRA_SUBJECT, args.name).putExtra(Intent.EXTRA_TEXT, args.text), null))
            invoke.resolve()
        } catch (_: Exception) { invoke.reject("errors.textShare") }
    }

    @Command
    fun saveAudio(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(AudioArgs::class.java)
            val file = privateFile(args.path)
            val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE)
                .setType(mime(file)).putExtra(Intent.EXTRA_TITLE, exportName(args.name, file.extension))
            startActivityForResult(invoke, intent, "saveAudioResult")
        } catch (_: Exception) { invoke.reject("errors.exportWrite") }
    }

    @ActivityCallback
    fun saveAudioResult(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode == Activity.RESULT_CANCELED) { invoke.resolve(); return }
        Thread({
            try {
                val uri = result.data?.data ?: throw IllegalStateException("No target")
                val source = privateFile(invoke.parseArgs(AudioArgs::class.java).path)
                activity.contentResolver.openOutputStream(uri, "w").use { destination ->
                    requireNotNull(destination)
                    source.inputStream().use { it.copyTo(destination) }
                    destination.flush()
                }
                invoke.resolve()
            } catch (_: Exception) { invoke.reject("errors.exportWrite") }
        }, "HearfolioAudioExport").start()
    }
}
