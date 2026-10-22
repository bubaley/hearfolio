// The recorder timeline is drawn on a canvas each animation frame. Bars and
// time marks are positioned by recording time, so they glide left from the
// centre playhead instead of being re-labelled in place.
export type RecorderPalette={bar:string;live:string;mark:string;label:string};

const HISTORY_MS=6000, MINOR_MS=500, MAJOR_MS=2000, RULER=20;

function clock(millis:number){const seconds=Math.floor(millis/1000);return `${Math.floor(seconds/60)}:${String(seconds%60).padStart(2,'0')}`;}

export function readPalette(element:Element):RecorderPalette {
  const style=getComputedStyle(element),value=(name:string,fallback:string)=>style.getPropertyValue(name).trim()||fallback;
  return {bar:value('--ink','#19191b'),live:value('--live','#e5483b'),mark:value('--line-strong','#cfcfca'),label:value('--faint','#9d9da4')};
}

export function drawRecorder(canvas:HTMLCanvasElement,levels:readonly number[],newestMillis:number,intervalMillis:number,nowMillis:number,paused:boolean,palette:RecorderPalette){
  const width=canvas.clientWidth,height=canvas.clientHeight;
  if(!width||!height)return;
  const ratio=window.devicePixelRatio||1,pixelWidth=Math.round(width*ratio),pixelHeight=Math.round(height*ratio);
  if(canvas.width!==pixelWidth||canvas.height!==pixelHeight){canvas.width=pixelWidth;canvas.height=pixelHeight;}
  const context=canvas.getContext('2d');if(!context)return;
  context.setTransform(ratio,0,0,ratio,0,0);context.clearRect(0,0,width,height);
  const centre=width/2,perMilli=centre/HISTORY_MS,top=RULER+8,waveHeight=height-top-6,middle=top+waveHeight/2;
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

  const step=intervalMillis*perMilli,bar=Math.max(1.5,Math.min(4,step*.62));
  context.fillStyle=palette.bar;
  const count=levels.length;
  for(let index=0;index<count;index++){
    const end=newestMillis-(count-1-index)*intervalMillis;
    if(end<=0)continue;
    const right=x(end);if(right<0)continue;
    const level=levels[index],barHeight=Math.max(2,level*waveHeight);
    context.globalAlpha=.85;
    context.beginPath();context.roundRect(right-bar,middle-barHeight/2,bar,barHeight,bar/2);context.fill();
  }
  // Native levels arrive a few hundred milliseconds late; bridge the gap to the playhead quietly.
  const newest=count?levels[count-1]:0;
  if(!paused&&newestMillis>0)for(let end=newestMillis+intervalMillis;end<nowMillis;end+=intervalMillis){
    const right=x(end),barHeight=Math.max(2,newest*waveHeight*.7);
    context.globalAlpha=.3;context.beginPath();context.roundRect(right-bar,middle-barHeight/2,bar,barHeight,bar/2);context.fill();
  }
  context.globalAlpha=1;
  context.fillStyle=paused?palette.label:palette.live;
  context.fillRect(Math.round(centre)-1,top-4,2,waveHeight+8);
  context.beginPath();context.arc(centre,top-4,3.5,0,Math.PI*2);context.fill();
}
