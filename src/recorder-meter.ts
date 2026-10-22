const levelValue=(value:number)=>Number.isFinite(value)?Math.max(0,Math.min(1,value)):0;
const sessionPath=(path:string|undefined)=>path?.endsWith('.wav.partial')?path.slice(0,-8):path;

/**
 * A display clock for native capture. Native time arrives every few hundred
 * milliseconds and slightly late; the display runs at real-time speed and is
 * gently pulled toward the measured value, so it never jumps, never runs
 * backwards and never invents audio when capture stalls.
 */
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
    // Never extrapolate more than 350 ms past the last measured sample.
    const target=this.measured+Math.min(350,Math.max(0,now-this.receivedAt)),elapsed=now-this.shownAt;
    if(this.shownAt<0||elapsed>120||elapsed<0)this.displayed=Math.max(this.displayed,target);
    else{
      // Behind: catch up up to 50% faster. Ahead (late delivery): slow to 70% instead of freezing.
      const rate=1+Math.max(-.3,Math.min(.5,(target-this.displayed)/300));
      const next=this.displayed+elapsed*rate;
      this.displayed=Math.max(this.displayed,Math.min(this.displayed<=target?target:this.measured+350,next));
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

/** Recorded samples advance with captured audio; pausing or hiding the UI keeps history. */
export class RecorderEnvelope {
  private values:number[];
  private lastSample=0;
  private session='';
  private sequence=-1;
  private newest=0;
  readonly size:number;
  readonly interval:number;
  constructor(size=64,interval=125){this.size=size;this.interval=interval;this.values=Array(size).fill(0);}
  reset(){this.values.fill(0);this.lastSample=0;this.session='';this.sequence=-1;this.newest=0;}
  /** `millis` is the recording time the sample belongs to, used to place it on the timeline. */
  sample(level:number,now:number,recording:boolean,millis?:number):boolean {
    if(!recording||(this.lastSample&&now-this.lastSample<this.interval))return false;
    // Unobserved time has no new samples. Keep the existing measured history.
    this.lastSample=now;this.values.shift();this.values.push(levelValue(level));this.newest=millis??this.newest+this.interval;return true;
  }
  synchronize(levels:readonly number[],sequence:number,session:string,interval=this.interval):boolean {
    if(!session||!Number.isSafeInteger(sequence)||sequence<0)return false;
    if(this.session===session&&sequence<=this.sequence)return false;
    if(this.session!==session){this.reset();this.session=session;}
    this.sequence=sequence;
    // Native buckets are numbered from the start of capture: bucket n ends at n × interval.
    this.newest=sequence*interval;
    const tail=levels.slice(-this.size).map(levelValue);
    this.values=[...Array(Math.max(0,this.size-tail.length)).fill(0),...tail];
    return true;
  }
  levels():readonly number[]{return this.values;}
  /** Recording time (ms) at which the newest level ends. */
  newestMillis(){return this.newest;}
}
