import test from 'node:test';
import assert from 'node:assert/strict';
import {RecorderEnvelope} from '../src/recorder-meter.ts';

test('recorder timeline holds measured samples, stays still while paused and does not invent silence motion', () => {
  const meter = new RecorderEnvelope(4, 100);
  meter.sample(.2, 100, true); meter.sample(.8, 200, true);
  assert.deepEqual(meter.levels(), [0,0,.2,.8]);
  assert.equal(meter.sample(1, 250, true), false);
  assert.equal(meter.sample(1, 400, false), false);
  assert.deepEqual(meter.levels(), [0,0,.2,.8]);
  meter.sample(0, 500, true);
  assert.deepEqual(meter.levels(), [0,.2,.8,0]);
});
test('resume clears unobserved waveform history and invalid input cannot overflow the display', () => {
  const meter = new RecorderEnvelope(3);
  meter.sample(1, 100, true); meter.sample(NaN, 2000, true);
  assert.deepEqual(meter.levels(), [0,0,0]);
  meter.sample(Infinity, 2300, true); meter.sample(9, 2600, true);
  assert.deepEqual(meter.levels(), [0,0,1]);
  meter.reset(); assert.deepEqual(meter.levels(), [0,0,0]);
});
