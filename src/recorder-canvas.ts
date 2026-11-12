// The recorder timeline is drawn on one canvas each animation frame. Bars and
// time marks are positioned by recording time, so they glide left from the
// centre playhead. Work and memory per frame are constant: only the buckets
// inside the visible window are visited, whatever the recording length.
export type RecorderPalette={bar:string;live:string;mark:string;label:string};
export type RecorderLevels={readonly size:number;readonly interval:number;readonly count:number;at(bucket:number):number};

const HISTORY_MS=6000, MINOR_MS=500, MAJOR_MS=2000, RULER=20, EDGE_FADE=36;
// Heights ease toward their measured value: a bar grows out of the playhead
// and a late native delivery settles in place instead of popping.
const RISE_MS=70, SETTLE_MS=140;

function clock(millis:number){
  const seconds=Math.floor(millis/1000),pad=(value:number)=>String(value).padStart(2,'0');
  return seconds>=3600?`${Math.floor(seconds/3600)}:${pad(Math.floor(seconds/60)%60)}:${pad(seconds%60)}`:`${Math.floor(seconds/60)}:${pad(seconds%60)}`;
}

export function readPalette(element:Element):RecorderPalette {
  const style=getComputedStyle(element),value=(name:string,fallback:string)=>style.getPropertyValue(name).trim()||fallback;
  return {bar:value('--ink','#19191b'),live:value('--live','#e5483b'),mark:value('--line-strong','#cfcfca'),label:value('--faint','#9d9da4')};
}

/**
 * Display state of the bars currently on screen, kept per absolute bucket in a
 * ring. Separated from drawing so the animation can be tested without a canvas.
 */
export class RecorderBars {
  private readonly shown:Float32Array;
  private readonly owner:Int32Array;
  private frameAt=-1;
  readonly size:number;
  constructor(size=64){this.size=size;this.shown=new Float32Array(size);this.owner=new Int32Array(size).fill(-1);}
  reset(){this.shown.fill(0);this.owner.fill(-1);this.frameAt=-1;}
  /**
   * Advances the animation to `frameTime` and reports each visible bucket:
   * `height` is the eased level, `measured` is false for the short stretch the
   * microphone has captured but not delivered yet.
   */
  frame(levels:RecorderLevels,nowMillis:number,paused:boolean,frameTime:number,draw:(bucket:number,height:number,measured:boolean)=>void){
    const fresh=this.frameAt<0,elapsed=fresh?0:Math.max(0,Math.min(100,frameTime-this.frameAt));this.frameAt=frameTime;
    const interval=levels.interval;
    // A bar appears once the playhead has passed its centre.
    const last=Math.floor((nowMillis-interval/2)/interval),first=Math.max(0,last-Math.min(this.size,Math.ceil(HISTORY_MS/interval)+2)+1);
    const pending=paused||levels.count===0?0:levels.at(levels.count-1)*.6;
    for(let bucket=first;bucket<=last;bucket++){
      const slot=bucket%this.size,measured=bucket<levels.count,target=measured?levels.at(bucket):pending;
      if(this.owner[slot]!==bucket){this.owner[slot]=bucket;this.shown[slot]=0;}
      const current=this.shown[slot];
      // The first frame after (re)attaching shows the history as it is, without replaying its growth.
      const ease=fresh?1:1-Math.exp(-elapsed/(target>current?RISE_MS:SETTLE_MS));
      const next=current+(target-current)*ease;
      this.shown[slot]=Math.abs(target-next)<.002?target:next;
      draw(bucket,this.shown[slot],measured);
    }
  }
}

/** Owns one canvas: its size is read on resize, never per frame. */
export class RecorderView {
  private canvas:HTMLCanvasElement|null=null;
  private context:CanvasRenderingContext2D|null=null;
  private observer:ResizeObserver|null=null;
  private palette:RecorderPalette|null=null;
  private width=0;
  private height=0;
  private ratio=1;
  private readonly bars=new RecorderBars();
  /** Call after every re-render; a new element replaces the previous one. */
  attach(canvas:HTMLCanvasElement|null){
    if(canvas===this.canvas){this.palette=null;return;}
    this.observer?.disconnect();this.observer=null;
    this.canvas=canvas;this.context=null;this.palette=null;this.width=0;this.height=0;this.bars.reset();
    if(!canvas)return;
    this.context=canvas.getContext('2d');
    this.measure();
    if(typeof ResizeObserver!=='undefined'){this.observer=new ResizeObserver(()=>this.measure());this.observer.observe(canvas);}
  }
  /** Forget the bar animation, e.g. when a new recording starts on the same element. */
  restart(){this.bars.reset();}
  private measure(){
    const canvas=this.canvas;if(!canvas)return;
    this.width=canvas.clientWidth;this.height=canvas.clientHeight;this.ratio=Math.min(3,window.devicePixelRatio||1);
    const pixelWidth=Math.round(this.width*this.ratio),pixelHeight=Math.round(this.height*this.ratio);
    if(canvas.width!==pixelWidth||canvas.height!==pixelHeight){canvas.width=pixelWidth;canvas.height=pixelHeight;}
  }
  draw(levels:RecorderLevels,nowMillis:number,paused:boolean,frameTime:number){
    const canvas=this.canvas,context=this.context;if(!canvas||!context)return;
    if(!this.width||!this.height){this.measure();if(!this.width||!this.height)return;}
    const palette=this.palette??=readPalette(canvas),width=this.width,height=this.height;
    context.setTransform(this.ratio,0,0,this.ratio,0,0);context.clearRect(0,0,width,height);
    const centre=width/2,perMilli=centre/HISTORY_MS,top=RULER+8,waveHeight=height-top-8,middle=top+waveHeight/2;
    const x=(millis:number)=>centre+(millis-nowMillis)*perMilli;

    // Time marks continue to the right so the playhead reads as "now" on a running tape.
    context.font='500 10px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif';
    context.textAlign='center';context.textBaseline='alphabetic';
    for(let mark=Math.max(0,Math.ceil((nowMillis-HISTORY_MS-MAJOR_MS)/MINOR_MS)*MINOR_MS);mark<=nowMillis+HISTORY_MS+MAJOR_MS;mark+=MINOR_MS){
      const position=x(mark);if(position<-30||position>width+30)continue;
      const major=mark%MAJOR_MS===0;
      context.fillStyle=palette.mark;context.fillRect(Math.round(position),RULER-(major?6:3),1,major?6:3);
      if(major){context.fillStyle=palette.label;context.fillText(clock(mark),position,RULER-9);}
    }

    const interval=levels.interval,step=interval*perMilli,bar=Math.max(1.5,Math.min(4,step*.6));
    context.fillStyle=palette.bar;
    this.bars.frame(levels,nowMillis,paused,frameTime,(bucket,level,measured)=>{
      const position=x((bucket+.5)*interval);if(position<-bar)return;
      const barHeight=Math.max(2,level*waveHeight);
      // History dissolves at the left edge instead of being cut off.
      context.globalAlpha=(measured?.88:.4)*Math.max(0,Math.min(1,position/EDGE_FADE));
      context.beginPath();context.roundRect(position-bar/2,middle-barHeight/2,bar,barHeight,bar/2);context.fill();
    });
    context.globalAlpha=1;
    context.fillStyle=paused?palette.label:palette.live;
    context.fillRect(Math.round(centre)-1,top-4,2,waveHeight+8);
    context.beginPath();context.arc(centre,top-4,3.5,0,Math.PI*2);context.fill();
  }
}
