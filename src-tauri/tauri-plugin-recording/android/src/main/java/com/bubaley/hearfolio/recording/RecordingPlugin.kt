package com.bubaley.hearfolio.recording

import android.Manifest
import android.app.Activity
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

@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone")])
class RecordingPlugin(private val activity: Activity) : Plugin(activity) {
    private var recorder: MediaRecorder? = null
    private var output: File? = null
    private var pending: Invoke? = null

    @Command
    fun start(invoke: Invoke) {
        if (recorder != null || pending != null) { invoke.reject("errors.operationBusy"); return }
        pending = invoke
        if (getPermissionState("microphone") != PermissionState.GRANTED) {
            requestPermissionForAlias("microphone", invoke, "microphonePermissionResult")
        } else { startGranted(invoke) }
    }

    @PermissionCallback
    fun microphonePermissionResult(invoke: Invoke) {
        if (pending !== invoke) { return }
        if (getPermissionState("microphone") != PermissionState.GRANTED) {
            pending = null
            invoke.reject("errors.microphonePermissionDenied")
        } else { startGranted(invoke) }
    }

    @Suppress("DEPRECATION")
    private fun startGranted(invoke: Invoke) {
        var next: MediaRecorder? = null
        try {
            val file = File(invoke.parseArgs(StartArgs::class.java).path).canonicalFile
            // Rust allocates the target under this app's private archive.
            val privateRoot = activity.dataDir.canonicalFile
            if (!file.path.startsWith(privateRoot.path + File.separator) || !file.name.startsWith("hearfolio-recording-")) {
                throw IllegalArgumentException("Invalid recording target")
            }
            file.parentFile?.mkdirs()
            output = file
            next = if (Build.VERSION.SDK_INT >= 31) MediaRecorder(activity) else MediaRecorder()
            next.setAudioSource(MediaRecorder.AudioSource.MIC)
            next.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            next.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            next.setAudioChannels(1)
            next.setAudioSamplingRate(44100)
            next.setAudioEncodingBitRate(96000)
            next.setOutputFile(file.path)
            next.prepare()
            next.start()
            recorder = next
            pending = null
            invoke.resolve()
        } catch (error: Exception) {
            next?.release()
            output?.delete(); output = null; pending = null
            invoke.reject(if (error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart")
        }
    }

    @Command
    fun stop(invoke: Invoke) {
        val active = recorder ?: run { invoke.reject("errors.recordingUnavailable"); return }
        recorder = null
        try {
            // MediaRecorder.stop throws if stopped before it produced samples.
            active.stop()
            if ((output?.length() ?: 0) <= 44) { throw IllegalStateException("Empty recording") }
            output = null
            invoke.resolve()
        } catch (error: Exception) {
            output?.delete(); output = null
            invoke.reject("errors.recordingEmpty")
        } finally { active.release() }
    }

    private fun cancelSession() {
        pending?.reject("errors.recordingUnavailable"); pending = null
        val active = recorder; recorder = null
        try { active?.stop() } catch (_: Exception) { }
        active?.release()
        output?.delete(); output = null
    }

    @Command
    fun cancel(invoke: Invoke) { cancelSession(); invoke.resolve() }

    override fun onDestroy(activity: AppCompatActivity) { cancelSession() }
}
