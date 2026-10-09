package com.bubaley.hearfolio.recording

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.*
import android.os.Build
import android.os.SystemClock
import android.view.View
import kotlin.math.*

// Quiet ambient voice envelopes behind the timer. Motion follows microphone input.
class AudioLevelView(context: Context) : View(context) {
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {style=Paint.Style.STROKE;strokeCap=Paint.Cap.ROUND;strokeJoin=Paint.Join.ROUND}
    private val curve=Path()
    private var target=0f
    private var shown=0f
    private var phase=0.0
    private var lastFrame=0L
    fun level(value:Float) {target=if(value.isFinite())value.coerceIn(0f,1f)else 0f;invalidate()}
    override fun onSizeChanged(w:Int,h:Int,oldw:Int,oldh:Int){
        super.onSizeChanged(w,h,oldw,oldh)
        paint.shader=LinearGradient(0f,0f,w.toFloat(),0f,intArrayOf(Color.TRANSPARENT,Color.argb(15,141,170,253),Color.argb(38,141,170,253),Color.argb(15,141,170,253),Color.TRANSPARENT),floatArrayOf(0f,.22f,.5f,.78f,1f),Shader.TileMode.CLAMP)
    }
    override fun onDraw(canvas:Canvas){
        super.onDraw(canvas)
        val now=SystemClock.uptimeMillis()
        val dt=if(lastFrame==0L)16L else(now-lastFrame).coerceIn(1L,64L);lastFrame=now
        val reduced=Build.VERSION.SDK_INT>=26&&!ValueAnimator.areAnimatorsEnabled()
        shown=if(reduced)target else shown+(target-shown)*(1-exp(-dt/(if(target>shown)90.0 else 260.0))).toFloat()
        if(abs(target-shown)<.001f)shown=target
        if(!reduced&&target>0f)phase+=dt*.0018*(.35+target)
        val density=resources.displayMetrics.density
        for(layer in 0..2){
            curve.reset()
            for(i in 0..80){
                val t=i/80.0
                val envelope=sin(PI*t).pow(2)
                val wave=sin(t*PI*(3.4+layer*.45)+phase*(1-layer*.12)+layer*1.4)
                val detail=sin(t*PI*7-phase*.6+layer)*.16
                val y=height/2f+(wave+detail)*shown*(height*60/220f)*envelope*(1-layer*.17)
                if(i==0)curve.moveTo(0f,y.toFloat())else curve.lineTo((t*width).toFloat(),y.toFloat())
            }
            paint.strokeWidth=10*density;paint.alpha=25;canvas.drawPath(curve,paint)
            paint.strokeWidth=1.1f*density;paint.alpha=255;canvas.drawPath(curve,paint)
        }
        if(!reduced&&(target>0f||shown>0f)&&isShown)postInvalidateOnAnimation() else lastFrame=0L
    }
}
