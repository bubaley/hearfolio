const levelValue=(value:number)=>Number.isFinite(value)?Math.max(0,Math.min(1,value)):0;
const sessionPath=(path:string|undefined)=>path?.endsWith('.wav.partial')?path.slice(0,-8):path;

/**
 * A display clock for native capture. Native time arrives every few hundred
 * milliseconds and slightly late; the display runs at real-time speed and is
 * gently pulled toward the measured value, so it never jumps, never runs
 * backwards and never invents audio when capture stalls.
 */
// Native time is polled about four times a second. One slow reply (a busy main thread, a
// collection pause) must not freeze the timer and the timeline with it, so the display may run
// this far past the last measurement before it waits. A capture that really stalls still stops.
const EXTRAPOLATE_MS=800;
export class NativeRecorderClock {
  private measured=0;
  private receivedAt=0;
  private running=false;
  private displayed=0;
  private shownAt=-1;
  reset(){this.measured=0;this.receivedAt=0;this.running=false;this.displayed=0;this.shownAt=-1;}
  synchronize(millis:number,state:string,now:number){
    if(!Number.isFinite(millis)||millis<0||!Number.isFinite(now))return;
    this.measured=millis;this.receivedAt=now;this.running=state==='recording';
    if(!this.running){this.displayed=millis;this.shownAt=now;}
  }
  milliseconds(now:number){
    if(!this.running){this.shownAt=now;return this.displayed=this.measured;}
    // Never extrapolate more than EXTRAPOLATE_MS past the last measured sample.
    const target=this.measured+Math.min(EXTRAPOLATE_MS,Math.max(0,now-this.receivedAt)),elapsed=now-this.shownAt;
    if(this.shownAt<0||elapsed>120||elapsed<0)this.displayed=Math.max(this.displayed,target);
    else{
      // Behind: catch up up to 50% faster. Ahead (late delivery): slow to 70% instead of freezing.
      const rate=1+Math.max(-.3,Math.min(.5,(target-this.displayed)/300));
      const next=this.displayed+elapsed*rate;
      this.displayed=Math.max(this.displayed,Math.min(this.displayed<=target?target:this.measured+EXTRAPOLATE_MS,next));
    }
    this.shownAt=now;
    return this.displayed;
  }
}

/** A control must read fresh state after an older in-flight snapshot, not skip it. */
export class RecordingSnapshotPoll {
  private active:Promise<void>|null=null;
  read(read:()=>Promise<void>,fresh=false):Promise<void>{
    if(this.active){
      return fresh?this.active.then(()=>this.read(read,true),()=>this.read(read,true)):this.active;
    }
    const active=Promise.resolve().then(read);
    this.active=active;
    return active.finally(()=>{if(this.active===active)this.active=null;});
  }
}

/** Orders native state and completion deliveries across pause and session changes. */
export class NativeRecordingOrder {
  private revision=-1;
  private path='';
  private starting=false;
  private startConfirmed=false;
  private live=false;
  beginStart(){this.starting=true;this.startConfirmed=false;}
  confirmStart(){this.startConfirmed=true;}
  abortStart(){this.starting=false;this.startConfirmed=false;}
  accept(payload:{state?:string;stateRevision?:number;path?:string}):boolean {
    payload={...payload,path:sessionPath(payload.path)};
    const revision=typeof payload.stateRevision==='number'&&Number.isSafeInteger(payload.stateRevision)?payload.stateRevision:undefined;
    const live=payload.state==='recording'||payload.state==='paused';
    const active=live||payload.state==='starting'||payload.state==='stopping';
    if(revision!==undefined&&revision<this.revision)return false;
    if(this.starting){
      const confirmedEnd=this.startConfirmed&&['stopping','finished','error','idle','cancelled'].includes(payload.state||'')&&revision!==undefined;
      if((!live&&!confirmedEnd)||!payload.path||payload.path===this.path||(revision!==undefined&&revision<=this.revision))return false;
      this.starting=false;
    }else if(payload.path&&this.path&&payload.path!==this.path){
      if(!active||revision===undefined||revision<=this.revision)return false;
    }
    if(revision!==undefined)this.revision=revision;
    if(payload.path)this.path=payload.path;
    this.live=active;
    return true;
  }
  acceptResult(payload:{stateRevision?:number;path?:string}):boolean {
    payload={...payload,path:sessionPath(payload.path)};
    // A durable completion may predate the service's acknowledgement. It still
    // belongs to the same ended session and must be restored after UI recreation.
    if(!this.starting&&!this.live&&payload.path&&(!this.path||payload.path===this.path)){this.path=payload.path;return true;}
    return this.accept({...payload,state:'finished'});
  }
}

/**
 * Measured levels in a fixed ring, addressed by absolute bucket number
 * (bucket n covers recording time n × interval … (n + 1) × interval).
 * Memory and work per update are constant for any recording length, and a bar
 * keeps its place on the timeline no matter when its value was delivered.
 * Pausing or hiding the UI keeps history; nothing is invented for unobserved time.
 */
export class RecorderEnvelope {
  private readonly ring:Float32Array;
  private end=0;
  private lastSample=0;
  private session='';
  private peak=0;
  private peakBucket=-1;
  readonly size:number;
  readonly interval:number;
  constructor(size=64,interval=125){this.size=size;this.interval=interval;this.ring=new Float32Array(size);}
  reset(){this.ring.fill(0);this.end=0;this.lastSample=0;this.session='';this.peak=0;this.peakBucket=-1;}
  /** Number of measured buckets since the start of the recording. */
  get count(){return this.end;}
  /** Level of an absolute bucket; 0 outside the retained window. */
  at(bucket:number):number {return bucket>=0&&bucket<this.end&&bucket>=this.end-this.size?this.ring[bucket%this.size]:0;}
  private write(bucket:number,level:number){
    // Buckets nobody measured (a throttled timer, a hidden window) stay silent.
    for(let gap=Math.max(this.end,bucket-this.size+1);gap<bucket;gap++)this.ring[gap%this.size]=0;
    this.ring[bucket%this.size]=level;this.end=bucket+1;
  }
  /** One measured level per call, at most once per interval of wall time `now`. */
  sample(level:number,now:number,recording:boolean):boolean {
    if(!recording||(this.lastSample&&now-this.lastSample<this.interval))return false;
    this.lastSample=now;this.write(this.end,levelValue(level));return true;
  }
  /**
   * Feeds a level observed at recording time `millis`. Calls may come at any
   * rate: the loudest level seen inside a bucket is committed when the bucket ends.
   */
  observe(level:number,millis:number):boolean {
    if(!Number.isFinite(millis)||millis<0)return false;
    const bucket=Math.floor(millis/this.interval);
    if(bucket<this.end)return false;
    let committed=false;
    if(bucket>this.peakBucket){
      if(this.peakBucket>=this.end){this.write(this.peakBucket,this.peak);committed=true;}
      this.peakBucket=bucket;this.peak=0;
    }
    this.peak=Math.max(this.peak,levelValue(level));
    return committed;
  }
  /** Native capture delivers its newest buckets; `sequence` is the number of buckets measured so far. */
  synchronize(levels:readonly number[],sequence:number,session:string,_interval=this.interval):boolean {
    if(!session||!Number.isSafeInteger(sequence)||sequence<0)return false;
    if(this.session===session&&sequence<=this.end)return false;
    if(this.session!==session){this.reset();this.session=session;}
    const count=Math.min(levels.length,this.size,sequence),first=sequence-count;
    for(let gap=Math.max(this.end,sequence-this.size);gap<first;gap++)this.ring[gap%this.size]=0;
    for(let index=0;index<count;index++)this.ring[(first+index)%this.size]=levelValue(levels[levels.length-count+index]);
    this.end=sequence;
    return true;
  }
  /** Retained levels, oldest first, padded with silence. */
  levels():readonly number[]{return Array.from({length:this.size},(_,index)=>this.at(this.end-this.size+index));}
  /** Recording time (ms) at which the newest level ends. */
  newestMillis(){return this.end*this.interval;}
}
