import {test} from 'node:test';
import assert from 'node:assert/strict';
import {applyQueueEvent, reconcileJobs, applyJobProgress, applyJobText, isProcessing, archiveBusy, queuePosition} from '../src/transcription-jobs.ts';

const progress = (recordId, current = 1, total = 4) => ({kind: 'transcribe', stage: 'progress.sendCloudChunk', current, total, elapsed: 3, recordId});

test('records move through queued, running and ended in the order the native queue reports', () => {
  const jobs = new Map();
  assert.equal(applyQueueEvent(jobs, {recordId: 'a', state: 'queued'}, 1), 'started');
  assert.equal(applyQueueEvent(jobs, {recordId: 'b', state: 'queued'}, 2), 'started');
  assert.equal(applyQueueEvent(jobs, {recordId: 'a', state: 'queued'}, 3), 'ignored');
  assert.equal(queuePosition(jobs, 'a'), 1);
  assert.equal(queuePosition(jobs, 'b'), 2);
  assert.equal(applyQueueEvent(jobs, {recordId: 'a', state: 'running'}, 4), 'changed');
  assert.equal(queuePosition(jobs, 'a'), 0);
  assert.equal(queuePosition(jobs, 'b'), 1);
  assert.equal(jobs.get('a').state, 'running');
  for (const state of ['finished', 'failed', 'cancelled']) {
    assert.equal(applyQueueEvent(jobs, {recordId: 'a', state}, 5), 'ended');
    assert.equal(applyQueueEvent(jobs, {recordId: 'a', state}, 5), 'ignored');
    applyQueueEvent(jobs, {recordId: 'a', state: 'queued'}, 6);
  }
});

test('progress and live text reach only the record they belong to', () => {
  const jobs = new Map();
  applyQueueEvent(jobs, {recordId: 'a', state: 'running'}, 1);
  applyQueueEvent(jobs, {recordId: 'b', state: 'queued'}, 1);
  assert.equal(applyJobProgress(jobs, progress('a', 2)), true);
  assert.equal(applyJobProgress(jobs, progress('zzz')), false);
  assert.equal(applyJobProgress(jobs, {...progress('a'), kind: 'download'}), false);
  assert.equal(applyJobProgress(jobs, {...progress('a'), recordId: null}), false);
  assert.equal(jobs.get('a').progress.current, 2);
  assert.equal(jobs.get('b').progress, null);
  assert.equal(applyJobText(jobs, 'a', 'hello'), true);
  assert.equal(applyJobText(jobs, 'gone', 'hello'), false);
  assert.equal(applyJobText(jobs, 'a', ''), false);
  assert.equal(jobs.get('a').text, 'hello');
});

test('a window that was away learns which records ended meanwhile', () => {
  const jobs = new Map();
  applyQueueEvent(jobs, {recordId: 'a', state: 'running'}, 1);
  applyQueueEvent(jobs, {recordId: 'b', state: 'queued'}, 1);
  const gone = reconcileJobs(jobs, [{recordId: 'b', state: 'running'}, {recordId: 'c', state: 'queued'}], 9);
  assert.deepEqual(gone, ['a']);
  assert.deepEqual([...jobs.keys()].sort(), ['b', 'c']);
  assert.equal(jobs.get('b').state, 'running');
  assert.equal(jobs.get('c').state, 'queued');
  assert.deepEqual(reconcileJobs(jobs, [], 10), ['b', 'c']);
  assert.equal(jobs.size, 0);
});

test('a record in the queue is protected and the archive stays put while anything is queued', () => {
  const jobs = new Map();
  assert.equal(archiveBusy(jobs), false);
  assert.equal(isProcessing(jobs, 'a'), false);
  assert.equal(isProcessing(jobs, null), false);
  applyQueueEvent(jobs, {recordId: 'a', state: 'queued'}, 1);
  assert.equal(isProcessing(jobs, 'a'), true);
  assert.equal(isProcessing(jobs, 'b'), false);
  assert.equal(archiveBusy(jobs), true);
});
