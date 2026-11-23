import {test} from 'node:test';
import assert from 'node:assert/strict';
import {transcriptParagraphs, speakerNames, speakerHeading, transcriptPlainLines, transcriptPlainText, formatClock, activeSegmentIndex} from './transcript.ts';

const seg = (start, end, text, speaker) => ({start_ms: start * 1000, end_ms: end * 1000, text, ...(speaker === undefined ? {} : {speaker})});

test('joins one speaker with short pauses into one text', () => {
  const result = transcriptParagraphs([seg(0, 6, 'Вы хотите сэкономить передачу и разархивацию...'), seg(6, 12, 'Сколько секунд? Пять? Десять?'), seg(12, 18, 'Пять? Десять? Сколько секунд? В')]);
  assert.equal(result.length, 1);
  assert.equal(result[0].start_ms, 0); assert.equal(result[0].end_ms, 18000);
  assert.equal(result[0].text, 'Вы хотите сэкономить передачу и разархивацию... Сколько секунд? Пять? Десять? Пять? Десять? Сколько секунд? В');
});

test('splits on a speaker change, long pauses and the 90 s cap', () => {
  const result = transcriptParagraphs([seg(0, 1, 'Первый.', '0:0'), seg(1.5, 2, 'Ещё.', '0:0'), seg(2, 3, 'Второй.', '0:1'), seg(5, 6, 'После паузы', '0:1'), seg(9.9, 10, 'дальше', '0:1'), seg(14, 15, 'и ещё', '0:1')]);
  assert.deepEqual(result.map(p => p.text), ['Первый. Ещё.', 'Второй.', 'После паузы дальше', 'и ещё']);
  assert.equal(transcriptParagraphs([seg(0, 300, 'Часть один'), seg(300, 600, 'часть два')]).length, 2);
});

test('drops empty segments and leaves paragraphs untouched', () => {
  const paragraphs = [seg(0, 10, 'Абзац.', '0:0'), seg(10, 20, 'Другой.', '0:1')];
  assert.deepEqual(transcriptParagraphs([seg(0, 1, '  '), ...paragraphs]), paragraphs);
});

test('numbers comparable speakers in order of appearance', () => {
  const names = speakerNames([seg(0, 1, 'a', '0:1'), seg(1, 2, 'b', '0:0'), seg(2, 3, 'c', '0:1')]);
  assert.deepEqual([...names], [['0:1', 'Спикер 1'], ['0:0', 'Спикер 2']]);
  assert.deepEqual(transcriptPlainLines([seg(0, 1, 'Привет.', '0:1'), seg(1, 2, 'Хорошо.', '0:0')]), ['Спикер 1: Привет.', 'Спикер 2: Хорошо.']);
});

test('names nobody for one speaker or labels of several requests', () => {
  assert.equal(speakerNames([seg(0, 1, 'a', '0:0'), seg(1, 2, 'b', '0:0')]).size, 0);
  assert.equal(speakerNames([seg(0, 1, 'a', '0:0'), seg(1, 2, 'b', '0:1'), seg(2, 3, 'c', '60000:0')]).size, 0);
  assert.equal(speakerNames([seg(0, 1, 'a'), seg(1, 2, 'b')]).size, 0);
});

test('shows the name only where the speaker changes', () => {
  const segments = [seg(0, 1, 'a', '0:0'), seg(1, 2, 'b', '0:0'), seg(2, 3, 'c', '0:1')];
  const names = speakerNames(segments);
  assert.deepEqual(segments.map((_, index) => speakerHeading(segments, names, index)), ['Спикер 1', undefined, 'Спикер 2']);
});

test('stored speaker names replace the numbers', () => {
  const segments = [seg(0, 1, 'Привет.', '0:1'), seg(1, 2, 'Да.', '0:0'), seg(2, 3, 'Ок.', '0:1')];
  const named = {'0:0': 'Аня'};
  assert.deepEqual([...speakerNames(segments, named).values()], ['Спикер 1', 'Аня']);
  assert.deepEqual(transcriptPlainLines(segments, named), ['Спикер 1: Привет.', 'Аня: Да.', 'Спикер 1: Ок.']);
  assert.equal(speakerNames(segments, {'0:0': '  '}).get('0:0'), 'Спикер 2');
});

test('plain text is speaker-labelled only when speakers are known', () => {
  assert.equal(transcriptPlainText([seg(0, 1, 'Раз.'), seg(1, 2, 'Два.')], 'Раз. Два.'), 'Раз. Два.');
  assert.equal(transcriptPlainText([seg(0, 1, 'Раз.', '0:0'), seg(1, 2, 'Два.', '0:1')], 'Раз. Два.'), 'Спикер 1: Раз.\nСпикер 2: Два.');
});

test('formats clocks and finds the playing paragraph', () => {
  assert.equal(formatClock(0), '0:00'); assert.equal(formatClock(65_000), '1:05'); assert.equal(formatClock(3_725_000), '1:02:05'); assert.equal(formatClock(-5), '0:00');
  const list = [seg(0, 5, 'a'), seg(10, 15, 'b'), seg(20, 25, 'c')];
  assert.equal(activeSegmentIndex(list, -1), -1);
  assert.equal(activeSegmentIndex(list, 0), 0);
  assert.equal(activeSegmentIndex(list, 9_999), 0);
  assert.equal(activeSegmentIndex(list, 10_000), 1);
  assert.equal(activeSegmentIndex(list, 99_000), 2);
  assert.equal(activeSegmentIndex([], 5), -1);
});
