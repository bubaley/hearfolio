import test from 'node:test';
import assert from 'node:assert/strict';
import {audioLevel, pcmLevel} from '../src/audio-level.ts';

test('silent and invalid input never animate the microphone meter', () => {
  for(const amplitude of [0,.00001,.001,NaN,Infinity,-1]) assert.equal(audioLevel(amplitude),0);
  assert.equal(pcmLevel(new Float32Array(4096)),0);
  assert.equal(pcmLevel(new Float32Array()),0);
});
test('recorded PCM tone envelope responds to loudness and is bounded', () => {
  const tone = amplitude => Float32Array.from({length:4096},(_,i)=>amplitude*Math.sin(2*Math.PI*440*i/44100));
  const quiet=pcmLevel(tone(.01)),speech=pcmLevel(tone(.1)),loud=pcmLevel(tone(.8));
  assert.ok(quiet>0 && quiet<speech && speech<loud && loud<=1);
  assert.equal(audioLevel(10),1);
});
