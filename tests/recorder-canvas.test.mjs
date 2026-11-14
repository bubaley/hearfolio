import {test} from 'node:test';
import assert from 'node:assert/strict';
import {RecorderBars} from '../src/recorder-canvas.ts';
import {RecorderEnvelope} from '../src/recorder-meter.ts';

const collect=(bars,levels,now,paused,time)=>{const drawn=new Map();bars.frame(levels,now,paused,time,(bucket,height,measured)=>drawn.set(bucket,{height,measured}));return drawn;};

test('a new bar grows out of the playhead and settles on its measured level',()=>{
  const levels=new RecorderEnvelope(64,125),bars=new RecorderBars(64);
  levels.synchronize([.5,.5,.5,.5],4,'a');
  collect(bars,levels,500,false,0);
  levels.synchronize([.8],5,'a');
  // Bucket 4 becomes visible once the playhead passes its centre (562.5 ms).
  let frame=collect(bars,levels,570,false,16);
  const born=frame.get(4).height;
  assert.ok(born>0&&born<.4,`starts small, got ${born}`);
  let previous=born;
  for(let time=32;time<=640;time+=16){frame=collect(bars,levels,570,false,time);const height=frame.get(4).height;assert.ok(height>=previous,'grows monotonically');previous=height;}
  assert.equal(previous,Math.fround(.8));
  assert.equal(frame.get(0).height,Math.fround(.5),'history is not disturbed');
});

test('captured but undelivered audio is bridged quietly and corrected in place',()=>{
  const levels=new RecorderEnvelope(64,125),bars=new RecorderBars(64);
  levels.synchronize([.6],8,'a');
  collect(bars,levels,1000,false,0);
  let frame;for(let time=16;time<=320;time+=16)frame=collect(bars,levels,1000+time,false,time);
  assert.equal(frame.get(8).measured,false);
  assert.ok(frame.get(8).height>.2&&frame.get(8).height<.4,'a subdued guess, not an invented peak');
  levels.synchronize([.6,1,.1],10,'a');
  const before=frame.get(8).height;
  frame=collect(bars,levels,1336,false,336);
  assert.equal(frame.get(8).measured,true);
  assert.ok(frame.get(8).height>before&&frame.get(8).height<1,'eases to the measured value instead of jumping');
  // While paused nothing is guessed.
  const paused=new RecorderBars(64);collect(paused,levels,2000,true,0);
  assert.equal(collect(paused,levels,2000,true,16).get(12).height,0);
});

test('a frame visits a bounded window after two hours of recording',()=>{
  const levels=new RecorderEnvelope(64,125),bars=new RecorderBars(64),sequence=2*60*60*8;
  levels.synchronize(Array(64).fill(.5),sequence,'long');
  const frame=collect(bars,levels,sequence*125+60,false,0);
  assert.ok(frame.size>=48&&frame.size<=52,`visited ${frame.size} buckets`);
  assert.equal(frame.get(sequence-1).height,.5);
});
