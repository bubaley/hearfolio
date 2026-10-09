package com.bubaley.hearfolio.recording

import android.Manifest
import android.app.PendingIntent
import android.content.ComponentName
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import androidx.core.content.ContextCompat

class RecordingTileService : TileService() {
    companion object {
        fun refresh(context: android.content.Context) { requestListeningState(context, ComponentName(context,RecordingTileService::class.java)) }
    }
    override fun onStartListening() {
        super.onStartListening()
        qsTile?.apply {
            state=if (RecordingService.state == "recording" || RecordingService.state == "paused") Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
            label=getString(R.string.hearfolio_record_tile)
            updateTile()
        }
    }
    @Suppress("DEPRECATION")
    private fun launchRecorder() {
        val intent=Intent(this,TileRecordingActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        if (Build.VERSION.SDK_INT >= 34) startActivityAndCollapse(PendingIntent.getActivity(this,4105,intent,PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE))
        else startActivityAndCollapse(intent)
    }
    override fun onClick() {
        super.onClick()
        // First-time microphone permission is requested on an unlocked screen.
        if (ContextCompat.checkSelfPermission(this,Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED && isLocked) unlockAndRun { launchRecorder() }
        else launchRecorder()
    }
}
