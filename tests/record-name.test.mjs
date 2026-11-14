import test from 'node:test';
import assert from 'node:assert/strict';
import {recordNameParts, renamedRecordTitle} from '../src/record-name.ts';

test('rename edits the title while retaining an audio extension and dots in the title', () => {
  const parts = recordNameParts('Встреча 12.10.M4A', '/archive/input/id.m4a');
  assert.deepEqual(parts, {title:'Встреча 12.10', extension:'.M4A'});
  assert.equal(renamedRecordTitle(' Итоги встречи ', parts.extension), 'Итоги встречи.M4A');
  assert.equal(renamedRecordTitle('Итоги.m4a', parts.extension), 'Итоги.M4A');
  assert.equal(renamedRecordTitle('   ', parts.extension), '');
});

test('legacy extensionless titles use the actual audio format without treating arbitrary dots as extensions', () => {
  assert.deepEqual(recordNameParts('План v2.0', '/archive/input/id.wav'), {title:'План v2.0', extension:'.wav'});
  assert.deepEqual(recordNameParts('Заметка', 'preview:id'), {title:'Заметка', extension:''});
  assert.equal(renamedRecordTitle('Заметка', ''), 'Заметка');
});
