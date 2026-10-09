// Layered voice envelopes with tapered edges, rather than an FFT spectrum.
export function recordingWavePath(level:number, phase:number, layer:number):string {
  const amplitude=Math.max(0,Math.min(1,level))*60;
  const points:string[]=[];
  for(let i=0;i<=80;i++){
    const t=i/80, envelope=Math.sin(Math.PI*t)**2;
    const wave=Math.sin(t*Math.PI*(3.4+layer*.45)+phase*(1-layer*.12)+layer*1.4);
    const detail=Math.sin(t*Math.PI*7-phase*.6+layer)*.16;
    const y=110+(wave+detail)*amplitude*envelope*(1-layer*.17);
    points.push(`${i===0?'M':'L'}${(t*640).toFixed(1)},${y.toFixed(2)}`);
  }
  return points.join(' ');
}
