package com.bubaley.hearfolio.recording

import android.Manifest
import android.app.Activity
import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.MediaPlayer
import android.media.MediaRecorder
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
class AudioArgs { lateinit var path: String; lateinit var name: String }
@InvokeArg
class TextArgs { lateinit var name: String; lateinit var text: String }
@InvokeArg
class SeekArgs { var seconds: Double = 0.0 }

@TauriPlugin(permissions = [Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone")])
class RecordingPlugin(private val activity: Activity) : Plugin(activity) {
    private var player: MediaPlayer? = null
    private var playbackGeneration = 0
    private var preparingPlayback: Invoke? = null
    private var recorder: AudioRecord? = null
    private var output: File? = null
    private var pending: Invoke? = null
    private var worker: Thread? = null
    @Volatile private var capturing = false
    @Volatile private var captureFailed = false
    @Volatile private var level = 0.0

    @Command
    fun start(invoke: Invoke) {
        if (recorder != null || pending != null || worker?.isAlive == true) { invoke.reject("errors.operationBusy"); return }
        releasePlayer()
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

    private fun privateFile(path: String): File {
        val file = File(path).canonicalFile
        require(file.path.startsWith(activity.dataDir.canonicalPath + File.separator))
        require(file.isFile)
        return file
    }

    @Suppress("DEPRECATION")
    private fun startGranted(invoke: Invoke) {
        var next: AudioRecord? = null
        try {
            val file = File(invoke.parseArgs(StartArgs::class.java).path).canonicalFile
            require(file.path.startsWith(activity.dataDir.canonicalPath + File.separator))
            require(file.name.startsWith("hearfolio-recording-") && file.extension == "wav")
            file.parentFile?.mkdirs()
            require(file.createNewFile())
            output = file
            val audioManager = activity.getSystemService(Context.AUDIO_SERVICE) as AudioManager
            val preferredSource = if (Build.VERSION.SDK_INT >= 24 && audioManager.getProperty(AudioManager.PROPERTY_SUPPORT_AUDIO_SOURCE_UNPROCESSED) == "true") {
                MediaRecorder.AudioSource.UNPROCESSED
            } else { MediaRecorder.AudioSource.VOICE_RECOGNITION }
            var actualRate = 0
            // Respect device capabilities without applying artificial gain to stored PCM.
            for (source in listOf(preferredSource, MediaRecorder.AudioSource.VOICE_RECOGNITION, MediaRecorder.AudioSource.MIC).distinct()) {
                for (rate in listOf(48000, 44100)) {
                    var candidate: AudioRecord? = null
                    try {
                        val minimum = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
                        require(minimum > 0)
                        candidate = AudioRecord(source, rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, maxOf(minimum * 4, rate / 5 * 2))
                        require(candidate.state == AudioRecord.STATE_INITIALIZED)
                        candidate.startRecording()
                        require(candidate.recordingState == AudioRecord.RECORDSTATE_RECORDING)
                        next = candidate; actualRate = rate
                        break
                    } catch (error: Exception) {
                        candidate?.release()
                        if (error is SecurityException) throw error
                    }
                }
                if (next != null) break
            }
            val active = requireNotNull(next)
            recorder = next
            capturing = true; captureFailed = false; level = 0.0
            worker = Thread({ capture(active, file, actualRate) }, "HearfolioAudioCapture").also { it.start() }
            pending = null
            invoke.resolve()
        } catch (error: Exception) {
            next?.release()
            output?.delete(); output = null; pending = null; recorder = null; capturing = false
            invoke.reject(if (error is SecurityException) "errors.microphonePermissionDenied" else "errors.recordingStart")
        }
    }

    // Store PCM directly; header duration is computed from captured samples, never wall time.
    private fun capture(active: AudioRecord, file: File, sampleRate: Int) {
        try {
            RandomAccessFile(file, "rw").use { wav ->
                wav.write(ByteArray(44))
                val samples = ShortArray(2048)
                val bytes = ByteArray(samples.size * 2)
                var count = 0L
                while (capturing) {
                    val read = active.read(samples, 0, samples.size, AudioRecord.READ_BLOCKING)
                    if (read < 0) { if (capturing) throw IllegalStateException("Capture failed"); break }
                    if (read == 0) continue
                    require(count + read * 2L <= 0xffffffffL - 36)
                    var energy = 0.0
                    for (index in 0 until read) {
                        val value = samples[index].toInt()
                        bytes[index * 2] = value.toByte()
                        bytes[index * 2 + 1] = (value shr 8).toByte()
                        val normalized = value / 32768.0
                        energy += normalized * normalized
                    }
                    level = sqrt(energy / read).coerceIn(0.0, 1.0)
                    wav.write(bytes, 0, read * 2)
                    count += read * 2L
                }
                wav.seek(0)
                fun little(value: Long, size: Int) { repeat(size) { wav.write((value shr (it * 8)).toInt() and 255) } }
                wav.writeBytes("RIFF"); little(count + 36, 4); wav.writeBytes("WAVEfmt ")
                little(16, 4); little(1, 2); little(1, 2); little(sampleRate.toLong(), 4)
                little(sampleRate * 2L, 4); little(2, 2); little(16, 2)
                wav.writeBytes("data"); little(count, 4); wav.fd.sync()
            }
        } catch (_: Exception) { captureFailed = true; capturing = false }
    }

    private fun finishCapture(): File? {
        capturing = false
        val active = recorder; recorder = null
        try { active?.stop() } catch (_: Exception) { captureFailed = true }
        worker?.join(250)
        active?.release()
        worker?.join(2000)
        if (worker?.isAlive == true) { captureFailed = true } else { worker = null }
        level = 0.0
        return output.also { output = null }
    }

    @Command
    fun stop(invoke: Invoke) {
        if (recorder == null) { invoke.reject("errors.recordingUnavailable"); return }
        val file = finishCapture()
        if (captureFailed || file == null || file.length() <= 44) {
            file?.delete()
            invoke.reject(if (captureFailed) "errors.recordingStop" else "errors.recordingEmpty")
        } else { invoke.resolve() }
    }

    @Command
    fun recordingLevel(invoke: Invoke) {
        if (captureFailed && recorder != null) { invoke.reject("errors.recordingStop"); return }
        invoke.resolve(JSObject().put("level", level))
    }

    private fun cancelSession() {
        pending?.reject("errors.recordingUnavailable"); pending = null
        finishCapture()?.delete()
    }
    @Command
    fun cancel(invoke: Invoke) { cancelSession(); invoke.resolve() }
    override fun onDestroy(activity: AppCompatActivity) { cancelSession(); releasePlayer() }

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
        if (recorder != null || pending != null) { invoke.reject("errors.operationBusy"); return }
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
