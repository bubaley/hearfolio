import {test} from 'node:test';
import assert from 'node:assert/strict';
import {Pacer} from '../src/pacer.ts';

const run=(pacer,from,to,step=16)=>{let value=0;for(let now=from;now<=to;now+=step)value=pacer.frame(now);return value;};

test('revealed text never runs ahead of what arrived',()=>{
  const pacer=new Pacer('reveal');pacer.reset(0,0);pacer.frame(0);
  pacer.set(100,3000);
  const early=run(pacer,3016,4500);
  assert.ok(early>35&&early<65,`the first burst is spread over the time it took, got ${early}`);
  assert.equal(run(pacer,4516,7000),100);
  assert.equal(run(pacer,7016,9000),100,'waits for the next burst');
});

test('each burst is spread over the measured arrival interval',()=>{
  const pacer=new Pacer('reveal');pacer.reset(0,0);pacer.frame(0);
  pacer.set(100,10000);run(pacer,16,20000);
  pacer.set(200,20000);
  const halfway=run(pacer,20016,25000);
  assert.ok(halfway>130&&halfway<170,`halfway through the interval about half is shown, got ${halfway}`);
  assert.equal(Math.round(run(pacer,25016,30100)),200);
});

test('finish flushes the remaining text quickly',()=>{
  const pacer=new Pacer('reveal');pacer.reset(0,0);pacer.frame(0);
  pacer.set(100,1000);run(pacer,16,30000);pacer.set(1000,30000);run(pacer,30016,31000);
  pacer.finish(31000);
  assert.equal(run(pacer,31016,32000),1000);
  assert.equal(pacer.pending,false);
});

test('predicted progress moves between updates but stops short of the next step',()=>{
  const pacer=new Pacer('predict');pacer.reset(0,0);pacer.frame(0);
  pacer.set(1,10000,1);pacer.set(2,20000,1);
  const moving=run(pacer,20016,25000);
  assert.ok(moving>2.2&&moving<3,`advances toward the next step, got ${moving}`);
  const waiting=run(pacer,25016,120000);
  assert.ok(waiting<3,'never claims the unconfirmed step');
  pacer.set(3,120000,1);
  assert.ok(run(pacer,120016,121000)>=waiting,'never moves backwards');
});

test('a smaller value restarts the pacer',()=>{
  const pacer=new Pacer('reveal');pacer.reset(50,0);pacer.set(10,100);
  assert.equal(pacer.frame(200),10);
});
