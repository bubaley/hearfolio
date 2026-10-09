package com.bubaley.hearfolio.recording

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.*
import android.view.Gravity
import android.view.WindowManager
import android.widget.*
import androidx.core.content.ContextCompat
import java.io.File
import java.util.Locale
import java.util.UUID

// Dedicated lock-screen surface: no WebView, archive, transcripts or settings.
class TileRecordingActivity : Activity() {
    private val handler=Handler(Looper.getMainLooper())
    private lateinit var status:TextView
    private lateinit var timer:TextView
    private lateinit var meter:AudioLevelView
    private lateinit var pause:Button
    private lateinit var save:Button
    private var requested=false
    private var starting=false
    private var awaitingFocus=false
    private var failure:String?=null
    private fun tr(ru:String,en:String)=if(Locale.getDefault().language=="ru") ru else en
    private fun dp(value:Int)=(value*resources.displayMetrics.density).toInt()
    private val refresh=object:Runnable { override fun run(){ update();handler.postDelayed(this,100) } }
    override fun onCreate(savedInstanceState:Bundle?) {
        super.onCreate(savedInstanceState)
        if(Build.VERSION.SDK_INT>=27){setShowWhenLocked(true);setTurnScreenOn(true)}
        else {window.addFlags(WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON)}
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        requested=savedInstanceState?.getBoolean("requested") ?: false
        val root=LinearLayout(this).apply{orientation=LinearLayout.VERTICAL;gravity=Gravity.CENTER;setPadding(dp(28),dp(24),dp(28),dp(24));setBackgroundColor(Color.rgb(33,33,35))}
        fun text(size:Float)=TextView(this).apply{setTextColor(Color.rgb(236,236,238));textSize=size;gravity=Gravity.CENTER;setPadding(0,dp(14),0,dp(14))}
        root.addView(text(26f).apply{text=tr("Запись Hearfolio","Hearfolio recording");setTypeface(null,Typeface.BOLD)})
        status=text(17f);root.addView(status)
        val recordingStage=FrameLayout(this)
        meter=AudioLevelView(this).apply{contentDescription=tr("Уровень микрофона","Microphone level")};recordingStage.addView(meter,FrameLayout.LayoutParams(-1,-1))
        timer=text(48f);recordingStage.addView(timer,FrameLayout.LayoutParams(-1,-1,Gravity.CENTER))
        root.addView(recordingStage,LinearLayout.LayoutParams(-1,dp(160)))
        fun button(primary:Boolean)=Button(this).apply{
            isAllCaps=false;textSize=17f;setTextColor(if(primary)Color.rgb(33,33,35)else Color.rgb(236,236,238))
            background=GradientDrawable().apply{cornerRadius=dp(12).toFloat();setColor(if(primary)Color.rgb(236,236,238)else Color.rgb(33,33,35));if(!primary)setStroke(dp(1),Color.rgb(65,65,69))}
            layoutParams=LinearLayout.LayoutParams(-1,dp(58)).apply{topMargin=dp(12)}
        }
        save=button(true).apply{text=tr("Остановить и сохранить","Stop and save");setOnClickListener{RecordingService.instance?.finish(true);update()}};root.addView(save)
        pause=button(false).apply{setOnClickListener{val action=if(RecordingService.state=="paused")RecordingService.RESUME else RecordingService.PAUSE;startService(Intent(this@TileRecordingActivity,RecordingService::class.java).setAction(action).putExtra("path",RecordingService.path))}};root.addView(pause)
        root.addView(button(false).apply{text=tr("Закрыть экран","Close screen");setOnClickListener{finish()}})
        root.addView(text(14f).apply{text=tr("Закрытие экрана не останавливает запись. Сохранённое аудио появится в архиве при открытии Hearfolio.","Closing this screen keeps recording. Saved audio appears in the archive when you open Hearfolio.");setTextColor(Color.rgb(156,156,166))})
        setContentView(ScrollView(this).apply{isFillViewport=true;addView(root)});update()
    }
    override fun onPostResume(){
        super.onPostResume();handler.post(refresh)
        if(!requested){requested=true;if(RecordingService.instance==null)requestRecording()}
    }
    private fun requestRecording(){
        if(ContextCompat.checkSelfPermission(this,Manifest.permission.RECORD_AUDIO)!=PackageManager.PERMISSION_GRANTED){requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO),1);return}
        if(Build.VERSION.SDK_INT>=33 && ContextCompat.checkSelfPermission(this,Manifest.permission.POST_NOTIFICATIONS)!=PackageManager.PERMISSION_GRANTED){requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS),2);return}
        begin()
    }
    override fun onRequestPermissionsResult(code:Int,permissions:Array<out String>,grants:IntArray){
        super.onRequestPermissionsResult(code,permissions,grants)
        if(code==1 && grants.firstOrNull()!=PackageManager.PERMISSION_GRANTED){failure=tr("Разрешите микрофон в настройках приложения.","Allow microphone access in app settings.");update();return}
        if(code==1)requestRecording() else if(code==2)begin()
    }
    override fun onWindowFocusChanged(hasFocus:Boolean){super.onWindowFocusChanged(hasFocus);if(hasFocus&&awaitingFocus)begin()}
    private fun begin(){
        if(!hasWindowFocus()){awaitingFocus=true;return}
        awaitingFocus=false
        if(starting || RecordingService.instance!=null)return
        starting=true
        val dir=File(filesDir,"hearfolio-quick-recordings").apply{mkdirs()}
        val target=File(dir,"hearfolio-recording-${UUID.randomUUID()}.wav.partial")
        RecordingService.ready={error->starting=false;if(error!=null)failure=tr("Не удалось начать запись. Откройте Hearfolio.","Could not start recording. Open Hearfolio.");update()}
        try {ContextCompat.startForegroundService(this,Intent(this,RecordingService::class.java).setAction(RecordingService.START).putExtra("path",target.path).putExtra("quick",true))}
        catch(_:Exception){starting=false;RecordingService.ready=null;failure=tr("Не удалось начать запись. Откройте Hearfolio.","Could not start recording. Open Hearfolio.");update()}
    }
    private fun update(){
        val state=RecordingService.state
        meter.level(if(state=="recording")RecordingService.inputLevel else 0f)
        status.text=failure ?: if(starting)tr("Подготовка…","Preparing…")else when(state){"recording"->tr("Идёт запись","Recording in progress");"paused"->tr("Запись на паузе","Recording paused");"finished"->tr("Запись сохранена","Recording saved");"error"->tr("Не удалось сохранить аудио","Could not save audio");else->tr("Запись завершена","Recording ended")}
        val seconds=RecordingService.elapsedSeconds();timer.text="%02d:%02d".format(seconds/60,seconds%60)
        val active=!starting && (state=="recording"||state=="paused") && RecordingService.instance!=null
        save.isEnabled=active;pause.isEnabled=active;save.alpha=if(active)1f else .4f;pause.alpha=save.alpha
        pause.text=if(state=="paused")tr("Продолжить","Resume")else tr("Пауза","Pause")
    }
    override fun onPause(){handler.removeCallbacks(refresh);super.onPause()}
    override fun onSaveInstanceState(outState:Bundle){outState.putBoolean("requested",requested);super.onSaveInstanceState(outState)}
}
