/**
 * Turns values that arrive in bursts (a finished segment of text, a chunk count)
 * into a steady flow. The pacer measures how long bursts take to arrive and
 * spreads each one over that interval, so the display keeps moving until the
 * next burst instead of jumping and then standing still.
 *
 * - `reveal` never shows more than has arrived (text characters).
 * - `predict` may run ahead toward the next expected step (progress), but never
 *   reaches it before it is confirmed.
 */
export type PacerMode='reveal'|'predict';
const EARLY_INTERVAL=1600, FLUSH_MS=700, PREDICT_SHARE=.92;

export class Pacer {
  private target=0;
  private shown=0;
  private rate=0;
  private interval=0;
  private updatedAt=0;
  private frameAt=0;
  private step=0;
  private finishing=false;
  private guess=0;
  private readonly mode:PacerMode;
  private readonly fallbackInterval:number;
  constructor(mode:PacerMode,fallbackInterval=EARLY_INTERVAL){this.mode=mode;this.fallbackInterval=fallbackInterval;}
  /** Jump to a value without animation, e.g. text restored after reopening the app. */
  reset(value=0,now=0){this.target=value;this.shown=value;this.rate=0;this.interval=0;this.updatedAt=now;this.frameAt=now;this.step=0;this.finishing=false;this.guess=0;}
  /** A new measured value. `nextStep` is the size of the step expected after it (predict mode). */
  set(value:number,now:number,nextStep?:number){
    if(!Number.isFinite(value))return;
    if(value<this.target){this.reset(value,now);return;}
    if(nextStep!==undefined&&nextStep>=0)this.step=nextStep;
    if(value===this.target)return;
    // The first interval runs from reset (the start of the job): how long the first burst took.
    const gap=Math.max(1,now-this.updatedAt);this.interval=this.interval?this.interval*.6+gap*.4:gap;
    if(nextStep===undefined)this.step=value-this.target;
    this.target=value;this.updatedAt=now;
    this.retime();
  }
  /** No more values will arrive: show what is left quickly but still smoothly. */
  finish(now:number){if(this.finishing)return;this.finishing=true;this.frameAt=this.frameAt||now;this.retime(FLUSH_MS);}
  /** Initial guess for how long one step takes, used until a real interval is measured. */
  expect(milliseconds:number){if(!this.interval&&milliseconds>0)this.guess=milliseconds;}
  /** Catch up within `window` ms, e.g. after the app returns from the background with a backlog. */
  hurry(window:number){this.retime(window);}
  /** finish() was called and everything that arrived is shown. */
  get finished(){return this.finishing&&this.shown>=this.target;}
  /** True while the shown value still has to catch up with what arrived. */
  get pending(){return this.shown<this.target;}
  get value(){return this.shown;}
  frame(now:number):number {
    const elapsed=this.frameAt?Math.max(0,Math.min(250,now-this.frameAt)):0;this.frameAt=now;
    if(this.mode==='reveal'||this.finishing){this.shown=Math.min(this.target,this.shown+this.rate*elapsed);return this.shown;}
    // Prediction eases toward a goal that slides forward with elapsed time but stays short of the next step.
    const expected=this.interval||this.guess||this.fallbackInterval*8,since=Math.max(0,now-this.updatedAt);
    const goal=this.target+this.step*PREDICT_SHARE*(1-Math.exp(-since/expected*1.6));
    const ease=1-Math.exp(-elapsed/220);
    this.shown=Math.max(this.shown,Math.min(goal,this.shown+(goal-this.shown)*ease));
    return this.shown;
  }
  private retime(window?:number){
    const backlog=this.target-this.shown;
    if(backlog<=0){this.rate=0;return;}
    // Spread what arrived over the time the next burst is expected to take.
    const span=window??(this.interval||this.fallbackInterval);
    this.rate=backlog/Math.max(60,span);
  }
}
