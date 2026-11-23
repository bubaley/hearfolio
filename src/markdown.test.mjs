import {test} from 'node:test';
import assert from 'node:assert/strict';
import {parseInline, parseMarkdown, safeHref, renderMarkdown, stripLeadingTitle} from './markdown.ts';

test('parses headings, paragraphs, rules and code', () => {
  const blocks = parseMarkdown('# Итоги\n\nПервая строка\nвторая\n\n---\n\n```\n<b>code</b>\n```');
  assert.deepEqual(blocks.map(block => block.type), ['heading', 'paragraph', 'hr', 'code']);
  assert.deepEqual(blocks[1], {type: 'paragraph', children: [{type: 'text', text: 'Первая строка'}, {type: 'br'}, {type: 'text', text: 'вторая'}]});
  assert.deepEqual(blocks[3], {type: 'code', text: '<b>code</b>'});
});

test('parses ordered, nested and task lists', () => {
  const [list] = parseMarkdown('3. one\n4. two\n   - nested\n   - more');
  assert.equal(list.type, 'list'); assert.equal(list.ordered, true); assert.equal(list.start, 3);
  assert.equal(list.items.length, 2);
  assert.deepEqual(list.items[1].map(block => block.type), ['paragraph', 'list']);
  const [tasks] = parseMarkdown('- [x] готово\n- [ ] нет\n- просто пункт');
  assert.deepEqual(tasks.checked, [true, false, null]);
});

test('parses quotes and tables', () => {
  const blocks = parseMarkdown('> цитата\n\n| Кто | Что |\n|---|:-:|\n| Аня | тексты |');
  assert.equal(blocks[0].type, 'quote');
  assert.equal(blocks[1].type, 'table'); assert.deepEqual(blocks[1].align, [null, 'center']);
});

test('parses inline marks and leaves snake_case alone', () => {
  assert.deepEqual(parseInline('**жирный** и *курсив*, `код`, ~~зачёркнутый~~').map(node => node.type), ['strong', 'text', 'em', 'text', 'code', 'text', 'del']);
  assert.deepEqual(parseInline('user_id and file_name'), [{type: 'text', text: 'user_id and file_name'}]);
});

test('links only safe URLs', () => {
  assert.deepEqual(parseInline('[плохая](javascript:alert(1))'), [{type: 'text', text: 'плохая'}]);
  assert.equal(safeHref(' https://a.b '), 'https://a.b');
  assert.equal(safeHref('mailto:a@b.c'), 'mailto:a@b.c');
  assert.equal(safeHref('data:text/html,x'), null);
  assert.equal(safeHref('/relative'), null);
  assert.deepEqual(parseInline('см. https://example.com/x.').map(node => node.type), ['text', 'link', 'text']);
});

test('renders HTML with every text node escaped', () => {
  assert.equal(renderMarkdown('# Итоги\n\n**жирно** и [доска](https://example.com/a?x=1&y=2)'),
    '<h1>Итоги</h1><p><strong>жирно</strong> и <a href="https://example.com/a?x=1&amp;y=2" target="_blank" rel="noopener noreferrer nofollow">доска</a></p>');
  assert.equal(renderMarkdown('- [x] a\n- b'), '<ul><li class="md-task is-done"><span class="md-check" role="img" aria-label="Выполнено">☑</span><div><p>a</p></div></li><li><p>b</p></li></ul>');
  assert.equal(renderMarkdown('```\n<b>x</b>\n```'), '<pre><code>&lt;b&gt;x&lt;/b&gt;</code></pre>');
  assert.equal(renderMarkdown('| A | B |\n|---|--:|\n| 1 | 2 |'), '<div class="md-table"><table><thead><tr><th>A</th><th style="text-align:right">B</th></tr></thead><tbody><tr><td>1</td><td style="text-align:right">2</td></tr></tbody></table></div>');
  assert.equal(renderMarkdown(null), '');
});

test('keeps raw HTML and dangerous links inert', () => {
  const html = renderMarkdown('<script>alert(1)</script> <img src=x onerror=alert(1)>');
  assert.equal(html, '<p>&lt;script&gt;alert(1)&lt;/script&gt; &lt;img src=x onerror=alert(1)&gt;</p>');
  assert.ok(!/<script|<img/i.test(html));
  const link = renderMarkdown('[клик](javascript:alert(1)) [x](data:text/html,<b>) [y](  JavaScript:alert(1))');
  assert.ok(!/href=/i.test(link)); assert.ok(!/<a /.test(link));
  assert.ok(!/href="javascript/i.test(renderMarkdown('[a](https://x.y/"onmouseover="alert(1))')));
  const quoted = renderMarkdown('[a](https://x.y/"onmouseover="alert(1)) <a href="javascript:1">');
  assert.ok(!/ onmouseover=/.test(quoted.replace(/&quot;/g, '')) || /&quot;onmouseover/.test(quoted));
  assert.ok(!/<a href="javascript/.test(quoted));
});

test('stripLeadingTitle drops a heading that repeats the title', () => {
  assert.equal(stripLeadingTitle('## Кратко\n\nКоманда обсудила сроки', 'Кратко'), 'Команда обсудила сроки');
  assert.equal(stripLeadingTitle('# «Решения и договорённости»:\nТекст', 'Решения и договорённости'), 'Текст');
  assert.equal(stripLeadingTitle('## Решения\n- пункт', 'Кратко'), '## Решения\n- пункт');
  assert.equal(stripLeadingTitle('Кратко\nтекст', 'Кратко'), 'Кратко\nтекст');
});
