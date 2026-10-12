import test from 'node:test';
import assert from 'node:assert/strict';
import {normalizeAudioCatalog, readAudioCatalog, saveAudioCatalog} from '../src/cloud-catalog.ts';

const storage = () => {
  let value = null;
  return {getItem: () => value, setItem: (_key, next) => {value = next;}};
};
const fish = {id: 'fish-audio/transcribe-1-pro', name: 'Fish', modes: ['transcription'], preferredMode: 'transcription'};

test('saved transcription catalog restores the supported endpoint after restart', () => {
  const cache = storage();
  saveAudioCatalog(cache, 'https://openrouter.ai/api/v1/', [fish]);
  assert.deepEqual(readAudioCatalog(cache, 'https://openrouter.ai/api/v1'), [fish]);
  assert.deepEqual(readAudioCatalog(cache, 'https://other.example/api/v1'), []);
});

test('malformed and unsupported catalog entries cannot enable recognition', () => {
  assert.deepEqual(normalizeAudioCatalog([null, {id: 'bad', name: 'Bad', modes: ['chat']}, {id: '', name: 'Bad', modes: ['streaming']}]), []);
  assert.deepEqual(normalizeAudioCatalog([{...fish, modes: ['streaming', 'transcription', 'streaming'], preferredMode: 'streaming'}])[0].preferredMode, 'transcription');
  assert.deepEqual(readAudioCatalog({getItem: () => '{broken', setItem() {}}, 'https://openrouter.ai/api/v1'), []);
});

test('storage failures do not break an available in-memory catalog', () => {
  const unavailable = {getItem() {throw Error('unavailable');}, setItem() {throw Error('full');}};
  assert.doesNotThrow(() => saveAudioCatalog(unavailable, 'https://openrouter.ai/api/v1', [fish]));
  assert.deepEqual(readAudioCatalog(unavailable, 'https://openrouter.ai/api/v1'), []);
});
