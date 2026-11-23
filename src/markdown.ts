/**
 * Tiny Markdown renderer for AI results (ported from Hearfolio Cloud): headings, paragraphs, (nested) lists, quotes,
 * code, rules, tables and the inline marks bold, italic, strikethrough, code and links. It parses to a tree and prints
 * HTML from it: every text node is escaped, so raw HTML in the source stays text, and only http(s)/mailto links
 * become links.
 */
import {escapeHtml as esc} from './icons.ts';

export type MdInline =
  | { type: 'text'; text: string }
  | { type: 'strong' | 'em' | 'del'; children: MdInline[] }
  | { type: 'code'; text: string }
  | { type: 'link'; href: string; children: MdInline[] }
  | { type: 'br' }

export type MdAlign = 'left' | 'center' | 'right' | null

export type MdBlock =
  | { type: 'heading'; level: 1 | 2 | 3 | 4 | 5 | 6; children: MdInline[] }
  | { type: 'paragraph'; children: MdInline[] }
  /** `checked[i]` is true/false for a task item (`- [x]` / `- [ ]`), null for a plain one. */
  | { type: 'list'; ordered: boolean; start: number; items: MdBlock[][]; checked: (boolean | null)[] }
  | { type: 'quote'; children: MdBlock[] }
  | { type: 'code'; text: string }
  | { type: 'hr' }
  | { type: 'table'; align: MdAlign[]; header: MdInline[][]; rows: MdInline[][][] }

const FENCE = /^\s{0,3}(`{3,}|~{3,})/
const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/
const RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/
const QUOTE = /^\s{0,3}>\s?/
const LIST_ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/
const TABLE_DIVIDER = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/

const isBlank = (line: string) => !line.trim()
const indentOf = (line: string) => line.match(/^\s*/)![0].replace(/\t/g, '  ').length

const isTableStart = (lines: string[], index: number) =>
  lines[index]!.includes('|') && index + 1 < lines.length && TABLE_DIVIDER.test(lines[index + 1]!)

const startsBlock = (lines: string[], index: number) => {
  const line = lines[index]!
  return (
    FENCE.test(line) ||
    HEADING.test(line) ||
    RULE.test(line) ||
    QUOTE.test(line) ||
    LIST_ITEM.test(line) ||
    isTableStart(lines, index)
  )
}

const SAFE_HREF = /^(https?:\/\/|mailto:)/i

/** Links are kept only for http(s) and mailto; anything else (javascript:, data:, relative) stays text. */
export const safeHref = (href: string): string | null => {
  const value = href.trim()
  return SAFE_HREF.test(value) ? value : null
}

const AUTOLINK = /^https?:\/\/[^\s<>()]*[^\s<>().,;:!?'"]/

const findClosing = (text: string, marker: string, from: number) => {
  let index = text.indexOf(marker, from)
  while (index !== -1 && text[index - 1] === '\\') index = text.indexOf(marker, index + 1)
  return index
}

export const parseInline = (text: string): MdInline[] => {
  const out: MdInline[] = []
  let buffer = ''
  const flush = () => {
    if (buffer) out.push({ type: 'text', text: buffer })
    buffer = ''
  }
  let i = 0
  while (i < text.length) {
    const char = text[i]!
    const rest = text.slice(i)

    if (char === '\\' && i + 1 < text.length) {
      const next = text[i + 1]!
      if (next === '\n') {
        flush()
        out.push({ type: 'br' })
        i += 2
        continue
      }
      if (/[!-/:-@[-`{-~]/.test(next)) {
        buffer += next
        i += 2
        continue
      }
    }
    if (char === '\n') {
      flush()
      out.push({ type: 'br' })
      i += 1
      continue
    }
    if (char === '`') {
      const run = rest.match(/^`+/)![0]
      const end = text.indexOf(run, i + run.length)
      if (end !== -1) {
        flush()
        out.push({ type: 'code', text: text.slice(i + run.length, end).trim() || text.slice(i + run.length, end) })
        i = end + run.length
        continue
      }
      buffer += run
      i += run.length
      continue
    }
    const double = rest.startsWith('**') ? '**' : rest.startsWith('__') ? '__' : rest.startsWith('~~') ? '~~' : null
    if (double && !(double === '__' && /\w/.test(text[i - 1] ?? ''))) {
      const end = findClosing(text, double, i + 2)
      if (end > i + 2) {
        flush()
        out.push({ type: double === '~~' ? 'del' : 'strong', children: parseInline(text.slice(i + 2, end)) })
        i = end + 2
        continue
      }
    }
    if ((char === '*' || char === '_') && !/\s/.test(text[i + 1] ?? ' ')) {
      const wordInside = char === '_' && /\w/.test(text[i - 1] ?? '')
      let end = findClosing(text, char, i + 1)
      // Skip a closing marker that is the start of a double one (`*a **b** c*`).
      while (end !== -1 && text[end + 1] === char) end = findClosing(text, char, end + 2)
      if (
        !wordInside &&
        end > i + 1 &&
        !/\s/.test(text[end - 1]!) &&
        !(char === '_' && /\w/.test(text[end + 1] ?? ''))
      ) {
        flush()
        out.push({ type: 'em', children: parseInline(text.slice(i + 1, end)) })
        i = end + 1
        continue
      }
    }
    if (char === '[') {
      const close = findClosing(text, ']', i + 1)
      if (close !== -1 && text[close + 1] === '(') {
        let depth = 0
        let end = -1
        for (let j = close + 2; j < text.length && end === -1; j += 1) {
          if (text[j] === '(') depth += 1
          else if (text[j] === ')') {
            if (depth === 0) end = j
            else depth -= 1
          }
        }
        if (end !== -1) {
          const label = text.slice(i + 1, close)
          const href = safeHref(text.slice(close + 2, end).split(/\s+/)[0] ?? '')
          flush()
          if (href) out.push({ type: 'link', href, children: parseInline(label) })
          else out.push(...parseInline(label))
          i = end + 1
          continue
        }
      }
    }
    if ((char === 'h' || char === 'H') && !/\w/.test(text[i - 1] ?? '')) {
      const match = rest.match(AUTOLINK)
      if (match) {
        flush()
        out.push({ type: 'link', href: match[0], children: [{ type: 'text', text: match[0] }] })
        i += match[0].length
        continue
      }
    }
    buffer += char
    i += 1
  }
  flush()
  return out
}

const splitRow = (line: string) => {
  let value = line.trim()
  if (value.startsWith('|')) value = value.slice(1)
  if (value.endsWith('|') && !value.endsWith('\\|')) value = value.slice(0, -1)
  const cells: string[] = []
  let cell = ''
  for (let i = 0; i < value.length; i += 1) {
    if (value[i] === '\\' && value[i + 1] === '|') {
      cell += '|'
      i += 1
    } else if (value[i] === '|') {
      cells.push(cell.trim())
      cell = ''
    } else cell += value[i]
  }
  cells.push(cell.trim())
  return cells
}

const TASK_MARK = /^\[([ xX])\]\s+/

const parseList = (lines: string[], start: number): { block: MdBlock; next: number } => {
  const first = lines[start]!.match(LIST_ITEM)!
  const baseIndent = indentOf(first[1]!)
  const ordered = /\d/.test(first[2]!)
  const items: string[][] = []
  let i = start
  let current: string[] | null = null
  let contentIndent = baseIndent + 2

  while (i < lines.length) {
    const line = lines[i]!
    const item = line.match(LIST_ITEM)
    if (item && indentOf(item[1]!) <= baseIndent + 1 && /\d/.test(item[2]!) === ordered) {
      current = [item[3]!]
      items.push(current)
      contentIndent = indentOf(item[1]!) + item[2]!.length + 1
      i += 1
      continue
    }
    if (!current) break
    if (isBlank(line)) {
      let peek = i + 1
      while (peek < lines.length && isBlank(lines[peek]!)) peek += 1
      if (peek < lines.length && indentOf(lines[peek]!) > baseIndent) {
        current.push('')
        i += 1
        continue
      }
      break
    }
    if (indentOf(line) > baseIndent) {
      current.push(line.slice(Math.min(indentOf(line), contentIndent)))
      i += 1
      continue
    }
    // Lazy continuation of the item's paragraph.
    if (!startsBlock(lines, i) && current.length && !isBlank(current[current.length - 1]!)) {
      current.push(line.trim())
      i += 1
      continue
    }
    break
  }
  const checked = items.map((item) => {
    const task = item[0]!.match(TASK_MARK)
    if (!task) return null
    item[0] = item[0]!.slice(task[0].length)
    return task[1] !== ' '
  })
  return {
    block: {
      type: 'list',
      ordered,
      start: ordered ? Number.parseInt(first[2]!, 10) : 1,
      items: items.map((item) => parseBlocks(item)),
      checked,
    },
    next: i,
  }
}

const parseBlocks = (lines: string[]): MdBlock[] => {
  const blocks: MdBlock[] = []
  let i = 0
  while (i < lines.length) {
    const line = lines[i]!
    if (isBlank(line)) {
      i += 1
      continue
    }
    const fence = line.match(FENCE)
    if (fence) {
      const body: string[] = []
      i += 1
      while (i < lines.length && !lines[i]!.trim().startsWith(fence[1]!)) body.push(lines[i++]!)
      blocks.push({ type: 'code', text: body.join('\n') })
      i += 1
      continue
    }
    const heading = line.match(HEADING)
    if (heading) {
      blocks.push({
        type: 'heading',
        level: heading[1]!.length as 1 | 2 | 3 | 4 | 5 | 6,
        children: parseInline(heading[2]!),
      })
      i += 1
      continue
    }
    if (RULE.test(line)) {
      blocks.push({ type: 'hr' })
      i += 1
      continue
    }
    if (QUOTE.test(line)) {
      const body: string[] = []
      while (i < lines.length && QUOTE.test(lines[i]!)) body.push(lines[i++]!.replace(QUOTE, ''))
      blocks.push({ type: 'quote', children: parseBlocks(body) })
      continue
    }
    if (LIST_ITEM.test(line)) {
      const { block, next } = parseList(lines, i)
      blocks.push(block)
      i = next
      continue
    }
    if (isTableStart(lines, i)) {
      const header = splitRow(line)
      const align = splitRow(lines[i + 1]!).map((cell): MdAlign => {
        const left = cell.startsWith(':')
        const right = cell.endsWith(':')
        return left && right ? 'center' : right ? 'right' : left ? 'left' : null
      })
      i += 2
      const rows: MdInline[][][] = []
      while (i < lines.length && !isBlank(lines[i]!) && lines[i]!.includes('|')) {
        const cells = splitRow(lines[i++]!)
        rows.push(header.map((_, index) => parseInline(cells[index] ?? '')))
      }
      blocks.push({ type: 'table', align, header: header.map((cell) => parseInline(cell)), rows })
      continue
    }
    const body: string[] = [line.trim()]
    i += 1
    while (i < lines.length && !isBlank(lines[i]!) && !startsBlock(lines, i)) body.push(lines[i++]!.trim())
    blocks.push({ type: 'paragraph', children: parseInline(body.join('\n')) })
  }
  return blocks
}

export const parseMarkdown = (source: string | null | undefined): MdBlock[] =>
  parseBlocks((source ?? '').replace(/\r\n?/g, '\n').split('\n'))

const inlineHtml = (nodes: MdInline[]): string =>
  nodes
    .map((node) => {
      switch (node.type) {
        case 'text':
          return esc(node.text)
        case 'br':
          return '<br>'
        case 'strong':
          return `<strong>${inlineHtml(node.children)}</strong>`
        case 'em':
          return `<em>${inlineHtml(node.children)}</em>`
        case 'del':
          return `<del>${inlineHtml(node.children)}</del>`
        case 'code':
          return `<code>${esc(node.text)}</code>`
        case 'link':
          return `<a href="${esc(node.href)}" target="_blank" rel="noopener noreferrer nofollow">${inlineHtml(node.children)}</a>`
      }
    })
    .join('')

const blockHtml = (node: MdBlock): string => {
  switch (node.type) {
    case 'heading':
      return `<h${node.level}>${inlineHtml(node.children)}</h${node.level}>`
    case 'paragraph':
      return `<p>${inlineHtml(node.children)}</p>`
    case 'hr':
      return '<hr>'
    case 'code':
      return `<pre><code>${esc(node.text)}</code></pre>`
    case 'quote':
      return `<blockquote>${node.children.map(blockHtml).join('')}</blockquote>`
    case 'list': {
      const tag = node.ordered ? 'ol' : 'ul'
      const start = node.ordered && node.start !== 1 ? ` start="${node.start}"` : ''
      const items = node.items
        .map((item, index) => {
          const checked = node.checked[index]
          const content = item.map(blockHtml).join('')
          if (checked === null || checked === undefined) return `<li>${content}</li>`
          return `<li class="md-task${checked ? ' is-done' : ''}"><span class="md-check" role="img" aria-label="${checked ? 'Выполнено' : 'Не выполнено'}">${checked ? '☑' : '☐'}</span><div>${content}</div></li>`
        })
        .join('')
      return `<${tag}${start}>${items}</${tag}>`
    }
    case 'table': {
      const align = (index: number) => {
        const value = node.align[index]
        return value ? ` style="text-align:${value}"` : ''
      }
      const head = node.header.map((cell, index) => `<th${align(index)}>${inlineHtml(cell)}</th>`).join('')
      const rows = node.rows
        .map((row) => `<tr>${row.map((cell, index) => `<td${align(index)}>${inlineHtml(cell)}</td>`).join('')}</tr>`)
        .join('')
      return `<div class="md-table"><table><thead><tr>${head}</tr></thead><tbody>${rows}</tbody></table></div>`
    }
  }
}

/** Markdown to a safe HTML string (see the header). */
export const renderMarkdown = (source: string | null | undefined): string => parseMarkdown(source).map(blockHtml).join('')

const comparable = (value: string) =>
  value
    .toLocaleLowerCase('ru')
    .replace(/[*_`«»"'“”.:!?]/g, '')
    .replace(/\s+/g, ' ')
    .trim()

/**
 * Summary Markdown without a leading heading that repeats the card title (models often start with `# Кратко`), so the
 * card does not say its name twice. Other headings stay.
 */
export const stripLeadingTitle = (content: string, title: string) => {
  const match = /^\s*#{1,3}\s+(.+?)\s*#*\s*(?:\n|$)/.exec(content)
  if (!match || comparable(match[1]!) !== comparable(title)) return content
  return content.slice(match[0].length).replace(/^\s*\n/, '')
}
