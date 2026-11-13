/**
 * Exactly one pending animation frame, however many callers ask for one.
 *
 * A frame requested while the WebView is in the background may never arrive,
 * so a request older than `stallMs` is replaced. The stale request is
 * cancelled first: left alone it would run later next to its replacement, and
 * every such pair would keep re-arming itself as a second, third, … loop —
 * a recorder left running behind a locked screen came back painting each
 * frame hundreds of times.
 */
export class FrameLoop {
  private handle:number|null=null;
  private requestedAt=0;
  private readonly paint:(now:number)=>boolean;
  private readonly schedule:(callback:(now:number)=>void)=>number;
  private readonly cancel:(handle:number)=>void;
  private readonly clock:()=>number;
  private readonly stallMs:number;
  /** `paint` returns true while it needs another frame. */
  constructor(paint:(now:number)=>boolean,schedule:(callback:(now:number)=>void)=>number=callback=>requestAnimationFrame(callback),cancel:(handle:number)=>void=handle=>cancelAnimationFrame(handle),clock:()=>number=()=>performance.now(),stallMs=500){
    this.paint=paint;this.schedule=schedule;this.cancel=cancel;this.clock=clock;this.stallMs=stallMs;
  }
  get pending(){return this.handle!==null;}
  request(){
    const now=this.clock();
    if(this.handle!==null){
      if(now-this.requestedAt<this.stallMs)return;
      this.cancel(this.handle);
    }
    this.requestedAt=now;
    const handle=this.schedule(time=>{
      // A cancelled frame that fires anyway must not start a parallel loop.
      if(this.handle!==handle)return;
      this.handle=null;
      if(this.paint(time))this.request();
    });
    this.handle=handle;
  }
  stop(){if(this.handle!==null)this.cancel(this.handle);this.handle=null;}
}
