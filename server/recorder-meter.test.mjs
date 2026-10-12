import test from 'node:test';
import assert from 'node:assert/strict';
import {RecorderEnvelope,NativeRecordingOrder} from '../src/recorder-meter.ts';

test('timeline advances only on measured samples and remains still during pause',()=>{
 const meter=new RecorderEnvelope(4,100);
 meter.sample(.2,100,true);meter.sample(.8,200,true);
 assert.deepEqual(meter.levels(),[0,0,.2,.8]);
 assert.equal(meter.sample(1,250,true),false);
 assert.equal(meter.sample(1,400,false),false);
 assert.deepEqual(meter.levels(),[0,0,.2,.8]);
 meter.sample(0,500,true);assert.deepEqual(meter.levels(),[0,.2,.8,0]);
});
test('unlock and long pause preserve existing history without invented background samples',()=>{
 const meter=new RecorderEnvelope(3);
 meter.sample(.4,100,true);meter.sample(.7,2000,true);
 assert.deepEqual(meter.levels(),[0,.4,.7]);
 meter.sample(NaN,2300,true);meter.sample(9,2600,true);
 assert.deepEqual(meter.levels(),[.7,0,1]);
 meter.reset();assert.deepEqual(meter.levels(),[0,0,0]);
});
test('native measured tail restores all background samples and ignores duplicate or older delivery',()=>{
 const meter=new RecorderEnvelope(4);
 assert.equal(meter.synchronize([.1,.5,.9],3,'session-a'),true);
 assert.deepEqual(meter.levels(),[0,.1,.5,.9]);
 assert.equal(meter.synchronize([1],2,'session-a'),false);
 assert.equal(meter.synchronize([1],3,'session-a'),false);
 assert.deepEqual(meter.levels(),[0,.1,.5,.9]);
 meter.synchronize([.1,.5,.9,.2,.8,.3],6,'session-a');
 assert.deepEqual(meter.levels(),[.9,.2,.8,.3]);
});
test('only a new native session resets history; invalid native samples remain bounded',()=>{
 const meter=new RecorderEnvelope(4);
 meter.synchronize([.7,.5],2,'session-a');
 meter.synchronize([Infinity,-2,4],3,'session-b');
 assert.deepEqual(meter.levels(),[0,0,0,1]);
 assert.equal(meter.synchronize([.5],NaN,'session-b'),false);
 assert.equal(meter.synchronize([.5],4,''),false);
});
test('native revisions reject a stale paused snapshot even when elapsed time has not changed',()=>{
 const order=new NativeRecordingOrder();
 assert.equal(order.accept({state:'paused',path:'a',stateRevision:2}),true);
 assert.equal(order.accept({state:'recording',path:'a',stateRevision:3}),true);
 assert.equal(order.accept({state:'paused',path:'a',stateRevision:2}),false);
 assert.equal(order.accept({state:'recording',path:'a',stateRevision:3}),true);
});
test('starting a new session ignores previous live and terminal deliveries',()=>{
 const order=new NativeRecordingOrder();
 order.accept({state:'recording',path:'a',stateRevision:1});
 order.beginStart();
 assert.equal(order.accept({state:'finished',path:'a',stateRevision:2}),false);
 assert.equal(order.accept({state:'recording',path:'a',stateRevision:1}),false);
 assert.equal(order.accept({state:'recording',path:'b',stateRevision:3}),true);
 assert.equal(order.accept({state:'finished',path:'a',stateRevision:4}),false);
 assert.equal(order.accept({state:'finished',path:'b',stateRevision:4}),true);
});
test('capture ending before the first UI poll restores its result only after start acknowledgement',()=>{
 const order=new NativeRecordingOrder();
 order.accept({state:'idle',path:'old',stateRevision:3});
 order.beginStart();
 assert.equal(order.accept({state:'finished',path:'new',stateRevision:5}),false);
 order.confirmStart();
 assert.equal(order.accept({state:'finished',path:'old',stateRevision:5}),false);
 assert.equal(order.accept({state:'idle',path:'new',stateRevision:5}),true);
 assert.equal(order.acceptResult({path:'new',stateRevision:5}),true);
});
test('quick recording terminal rename keeps the same session without accepting a different recording',()=>{
 const order=new NativeRecordingOrder();
 assert.equal(order.accept({state:'recording',path:'quick.wav.partial',stateRevision:1}),true);
 assert.equal(order.accept({state:'stopping',path:'quick.wav.partial',stateRevision:2}),true);
 assert.equal(order.accept({state:'finished',path:'quick.wav',stateRevision:3}),true);
 assert.equal(order.acceptResult({path:'quick.wav',stateRevision:3}),true);
 assert.equal(order.accept({state:'finished',path:'other.wav',stateRevision:4}),false);
});
test('durable completed recording survives service acknowledgement without closing a newer live session',()=>{
 const order=new NativeRecordingOrder();
 order.accept({state:'idle',stateRevision:4});
 assert.equal(order.acceptResult({path:'a',stateRevision:3}),true);
 order.accept({state:'recording',path:'b',stateRevision:5});
 assert.equal(order.acceptResult({path:'a',stateRevision:3}),false);
});
