package com.bubaley.hearfolio.recording

import android.Manifest
import android.app.Activity
import android.content.Intent
import androidx.core.content.ContextCompat
import app.tauri.plugin.JSObject
import android.media.MediaRecorder
import android.os.Build
import androidx.appcompat.app.AppCompatActivity
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.io.File

@InvokeArg
class StartArgs { lateinit var path: String }

@InvokeArg
class TransferArgs {
    lateinit var id: String
    var direction: String = "send"
    var stage: String = "waiting"
    var current: Long = 0
    var total: Long = 0
    var success: Boolean = false
}

@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone"), Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications")])
class RecordingPlugin(private val activity: Activity) : Plugin(activity) {
    private var pending: Invoke? = null

    @Command
    fun start(invoke: Invoke) {
        if (RecordingService.instance != null || pending != null) { invoke.reject("errors.operationBusy"); return }
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
    fun recordingNotificationResult(invoke: Invoke) { startGranted(invoke) }
    @Command
    fun acknowledgeRecording(invoke: Invoke) { activity.stopService(Intent(activity, RecordingService::class.java)); invoke.resolve() }

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
        if (RecordingService.state == "finished") { activity.stopService(Intent(activity, RecordingService::class.java)); invoke.resolve(); return }
        val error = RecordingService.instance?.finish(false) ?: "errors.recordingUnavailable"
        if (error.isEmpty()) invoke.resolve() else invoke.reject(error)
    }
    @Command
    fun cancel(invoke: Invoke) {
        pending?.reject("errors.recordingUnavailable"); pending = null
        RecordingService.instance?.discard(); invoke.resolve()
    }
    @Command
    fun recordingStatus(invoke: Invoke) {
        val result = JSObject(); result.put("state", RecordingService.state); result.put("path", RecordingService.path); result.put("elapsedSeconds", RecordingService.elapsedSeconds()); result.put("quick", RecordingService.quickRecording); result.put("level", RecordingService.inputLevel); invoke.resolve(result)
    }
    @Command
    fun quickControl(invoke: Invoke) {
        if (!RecordingService.quickRecording) { invoke.reject("errors.recordingUnavailable"); return }
        val action=invoke.parseArgs(TransferArgs::class.java).stage
        when(action) {
            "stop" -> { if (RecordingService.state == "finished") invoke.resolve() else { val error=RecordingService.instance?.finish(true) ?: "errors.recordingUnavailable"; if(error.isEmpty())invoke.resolve() else invoke.reject(error) } }
            "cancel" -> {RecordingService.instance?.discard();invoke.resolve()}
            else -> invoke.reject("errors.recordingUnavailable")
        }
    }

    @Command
    fun quickRecordings(invoke: Invoke) {
        val dir=File(activity.filesDir,"hearfolio-quick-recordings")
        val files=dir.listFiles()?.filter { it.isFile && it.canonicalFile.parentFile==dir.canonicalFile && it.name.startsWith("hearfolio-recording-") && it.name.endsWith(".m4a") }?.map { it.canonicalPath } ?: emptyList()
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
            ContextCompat.startForegroundService(activity, Intent(activity, InboxService::class.java).setAction(InboxService.START))
        } catch (_: Exception) { InboxService.ready = null; invoke.reject("errors.transferBackgroundUnavailable") }
    }
    @Command
    fun stopInbox(invoke: Invoke) { activity.stopService(Intent(activity, InboxService::class.java)); invoke.resolve() }
    @Command
    fun inboxStatus(invoke: Invoke) {
        val result = JSObject(); result.put("active", InboxService.active); invoke.resolve(result)
    }
    @Command
    fun inboxOffers(invoke: Invoke) {
        val args = invoke.parseArgs(TransferArgs::class.java)
        InboxService.instance?.offers(args.current.toInt(), args.success)
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
            if (InboxService.active) { InboxService.instance?.transfer(true, false, args.direction); invoke.resolve(); return }
            TransferService.ready = { error -> if (error == null) invoke.resolve() else invoke.reject(error) }
            ContextCompat.startForegroundService(activity, Intent(activity, TransferService::class.java).setAction(TransferService.BEGIN).putExtra("id",args.id).putExtra("direction",args.direction))
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

    override fun onDestroy(activity: AppCompatActivity) { /* Services own recording and transfer lifetimes. */ }
}
