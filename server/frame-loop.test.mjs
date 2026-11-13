import {test} from 'node:test';
import assert from 'node:assert/strict';
import {FrameLoop} from '../src/frame-loop.ts';

// A browser-like scheduler: callbacks queue up while "hidden" and all run on the next visible frame.
function scheduler(){
  let next=1,now=0;const queue=new Map();
  return {
    schedule:callback=>{const id=next++;queue.set(id,callback);return id;},
    cancel:id=>{queue.delete(id);},
    clock:()=>now,
    advance:ms=>{now+=ms;},
    frame(){const due=[...queue.values()];queue.clear();for(const callback of due)callback(now);return due.length;},
    get queued(){return queue.size;},
  };
}

test('many requests share one frame and a running loop re-arms exactly once',()=>{
  const s=scheduler();let painted=0;
  const loop=new FrameLoop(()=>{painted++;return true;},s.schedule,s.cancel,s.clock);
  loop.request();loop.request();loop.request();
  assert.equal(s.queued,1);
  for(let frame=0;frame<100;frame++){s.advance(16);assert.equal(s.frame(),1);loop.request();}
  assert.equal(painted,100);assert.equal(s.queued,1);
});

test('hours of requests behind a locked screen never multiply the loop',()=>{
  const s=scheduler();let painted=0;
  const loop=new FrameLoop(()=>{painted++;return true;},s.schedule,s.cancel,s.clock);
  loop.request();
  // Two hours hidden: native state arrives twice a second, frames never fire.
  for(let tick=0;tick<14400;tick++){s.advance(500);loop.request();}
  assert.equal(s.queued,1,'stale requests are replaced, not accumulated');
  s.advance(16);assert.equal(s.frame(),1);
  assert.equal(painted,1);
  for(let frame=0;frame<10;frame++){s.advance(16);assert.equal(s.frame(),1);}
  assert.equal(painted,11);
});

test('a cancelled frame that still fires cannot start a second loop',()=>{
  let now=0;const callbacks=[];let painted=0;
  // This scheduler ignores cancellation, like a frame already being dispatched.
  const loop=new FrameLoop(()=>{painted++;return true;},callback=>{callbacks.push(callback);return callbacks.length;},()=>{},()=>now);
  loop.request();now=600;loop.request();
  assert.equal(callbacks.length,2);
  callbacks[0](now);callbacks[1](now);
  assert.equal(painted,1);assert.equal(callbacks.length,3);
});

test('the loop ends when nothing needs painting and stop cancels a pending frame',()=>{
  const s=scheduler();let painted=0;
  const loop=new FrameLoop(()=>{painted++;return false;},s.schedule,s.cancel,s.clock);
  loop.request();s.frame();
  assert.equal(painted,1);assert.equal(s.queued,0);assert.equal(loop.pending,false);
  loop.request();loop.stop();
  assert.equal(s.queued,0);assert.equal(s.frame(),0);
});
