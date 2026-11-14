import {test} from 'node:test';
import assert from 'node:assert/strict';
import {describeBusy, busyPercent} from '../src/busy-activity.ts';

const names = id => id === 'hearing-1' ? 'Встреча 12.11' : null;

test('a running transcription says what and how far, and can be opened or stopped', () => {
  const busy = {kind: 'transcribe', seconds: 90, stopping: false, operation: {id: 'op', kind: 'transcribe', recordId: 'hearing-1', progress: {stage: 'progress.recognizedChunks', current: 9, total: 20}}};
  const ru = describeBusy(busy, names, 'ru');
  assert.equal(ru.message, 'Идёт расшифровка «Встреча 12.11» — 45%. Дождитесь окончания или остановите её.');
  assert.deepEqual(ru.open, {target: 'record', recordId: 'hearing-1'});
  assert.deepEqual(ru.stop, {target: 'operation', id: 'op'});
  assert.match(describeBusy(busy, names, 'en').message, /^Transcribing “Встреча 12\.11” — 45%\. Wait/);
});

test('a stop in progress is not offered again and the record name is optional', () => {
  const stopping = describeBusy({kind: 'transcribe', stopping: true, operation: {id: 'op', kind: 'transcribe', recordId: 'gone'}}, names, 'ru');
  assert.match(stopping.message, /^Останавливаем текущую задачу/);
  assert.equal(stopping.stop, undefined);
  const unnamed = describeBusy({kind: 'postprocess', operation: {id: 'op', kind: 'postprocess', recordId: 'gone'}}, names, 'en');
  assert.match(unnamed.message, /^Processing the text of a recording\./);
});

test('every holder gets a plain message; only stoppable work offers Stop', () => {
  for (const language of ['ru', 'en']) {
    for (const kind of ['idle', 'transcribe', 'postprocess', 'import', 'recording', 'download', 'storage', 'archive', 'restart', 'something-new']) {
      const notice = describeBusy({kind}, names, language);
      assert.ok(notice.message.length > 10, `${language}:${kind}`);
      assert.doesNotMatch(notice.message, /errors\.|undefined|null|NaN/);
    }
  }
  assert.deepEqual(describeBusy({kind: 'recording'}, names, 'ru').stop, {target: 'recording'});
  assert.equal(describeBusy({kind: 'download'}, names, 'ru').stop, undefined);
  assert.equal(describeBusy({kind: 'archive'}, names, 'ru').stop, undefined);
  assert.equal(describeBusy({kind: 'idle'}, names, 'ru').open, undefined);
});

test('progress never reads as finished while the job is still running', () => {
  assert.equal(busyPercent(null), null);
  assert.equal(busyPercent({current: 0, total: 0}), null);
  assert.equal(busyPercent({current: 1, total: 3}), 33);
  assert.equal(busyPercent({current: 5, total: 5}), 99);
  assert.equal(busyPercent({current: -2, total: 5}), 0);
});
