// Custom select: every native <select> in the app gets a styled trigger and a
// list popover with search, matching the cloud web. The native element stays in
// the DOM (hidden) as the source of truth, so existing `change` listeners,
// `.value` reads and form data keep working unchanged.
import {escapeHtml as esc, icon} from './icons';
import {currentLanguage} from './i18n';

const ENHANCED = 'appSelect';
const SEARCH_FROM = 8; // short lists do not need a search field
let open: {select: HTMLSelectElement; trigger: HTMLButtonElement; panel: HTMLElement; cleanup: () => void} | null = null;

const optionsOf = (select: HTMLSelectElement) => [...select.options].filter(option => !option.hidden);
const labelOf = (select: HTMLSelectElement) => select.selectedOptions[0]?.textContent?.trim() || '';

function syncTrigger(select: HTMLSelectElement, trigger: HTMLButtonElement) {
  const label = trigger.querySelector<HTMLElement>('.app-select-label');
  if (label) label.textContent = labelOf(select);
  trigger.disabled = select.disabled;
  trigger.title = labelOf(select);
}

function close(restoreFocus = true) {
  if (!open) return;
  const current = open;
  open = null;
  current.cleanup();
  current.panel.remove();
  current.trigger.setAttribute('aria-expanded', 'false');
  if (restoreFocus && current.trigger.isConnected) current.trigger.focus();
}

function choose(select: HTMLSelectElement, trigger: HTMLButtonElement, value: string) {
  close();
  if (select.value === value) return;
  select.value = value;
  syncTrigger(select, trigger);
  select.dispatchEvent(new Event('input', {bubbles: true}));
  select.dispatchEvent(new Event('change', {bubbles: true}));
}

function place(panel: HTMLElement, trigger: HTMLElement) {
  // A bare select lives inside a chip: the list hangs from the chip, not from the text inside it.
  const box = (trigger.closest('.model-chip') || trigger).getBoundingClientRect(), gap = 6, margin = 8;
  const width = Math.min(Math.max(box.width, 220), window.innerWidth - margin * 2);
  panel.style.width = `${width}px`;
  panel.style.left = `${Math.min(Math.max(margin, box.left), window.innerWidth - width - margin)}px`;
  const below = window.innerHeight - box.bottom - margin, above = box.top - margin;
  const height = Math.min(panel.scrollHeight, 320);
  // Below when the whole list fits there; otherwise on the side with more room, so it is never cut short needlessly.
  const limit = (room: number) => `${Math.max(120, Math.min(360, room - gap))}px`;
  // The side is chosen once: a list that shrinks while you type must not jump across its field.
  const side = panel.dataset.side || (below >= height + gap || below >= above ? 'below' : 'above');
  panel.dataset.side = side;
  if (side === 'below') {panel.style.top = `${box.bottom + gap}px`;panel.style.bottom = '';panel.style.maxHeight = limit(below);}
  else {panel.style.bottom = `${window.innerHeight - box.top + gap}px`;panel.style.top = '';panel.style.maxHeight = limit(above);}
}

function openPanel(select: HTMLSelectElement, trigger: HTMLButtonElement) {
  if (select.disabled) return;
  close(false);
  const options = optionsOf(select), searchable = options.length >= SEARCH_FROM, ru = currentLanguage() === 'ru';
  const panel = document.createElement('div');
  panel.className = 'app-select-panel';
  panel.innerHTML = `${searchable ? `<label class="app-select-search">${icon('search')}<input type="text" autocomplete="off" spellcheck="false" placeholder="${ru ? 'Поиск…' : 'Search…'}" aria-label="${ru ? 'Поиск' : 'Search'}"></label>` : ''}<div class="app-select-list" role="listbox"></div><div class="app-select-empty" hidden>${ru ? 'Ничего не найдено' : 'Nothing found'}</div>`;
  const list = panel.querySelector<HTMLElement>('.app-select-list')!, empty = panel.querySelector<HTMLElement>('.app-select-empty')!;
  const search = panel.querySelector<HTMLInputElement>('input');
  let active = Math.max(0, options.findIndex(option => option.selected));
  let visible = options;
  const render = () => {
    const query = search?.value.trim().toLocaleLowerCase() || '';
    visible = query ? options.filter(option => (option.textContent || '').toLocaleLowerCase().includes(query)) : options;
    if (active >= visible.length) active = Math.max(0, visible.length - 1);
    list.innerHTML = visible.map((option, index) => `<button type="button" role="option" class="app-select-option ${option.selected ? 'selected' : ''} ${index === active ? 'active' : ''}" aria-selected="${option.selected}" data-index="${index}" ${option.disabled ? 'disabled' : ''}><span>${esc(option.textContent?.trim() || '')}</span>${option.selected ? icon('check') : ''}</button>`).join('');
    empty.hidden = visible.length > 0;
  };
  const highlight = (index: number) => {
    if (!visible.length) return;
    active = (index + visible.length) % visible.length;
    list.querySelectorAll('.app-select-option').forEach((element, i) => element.classList.toggle('active', i === active));
    list.querySelector<HTMLElement>(`[data-index="${active}"]`)?.scrollIntoView({block: 'nearest'});
  };
  render();
  list.addEventListener('mousedown', event => event.preventDefault());
  list.addEventListener('click', event => {
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>('.app-select-option');
    if (button && !button.disabled) choose(select, trigger, visible[Number(button.dataset.index)].value);
  });
  list.addEventListener('mousemove', event => {
    const button = (event.target as HTMLElement).closest<HTMLElement>('.app-select-option');
    if (button && Number(button.dataset.index) !== active) highlight(Number(button.dataset.index));
  });
  search?.addEventListener('input', () => {active = 0;render();place(panel, trigger);});
  panel.addEventListener('keydown', event => {
    if (event.key === 'ArrowDown') {event.preventDefault();highlight(active + 1);}
    else if (event.key === 'ArrowUp') {event.preventDefault();highlight(active - 1);}
    else if (event.key === 'Home' && !search) {event.preventDefault();highlight(0);}
    else if (event.key === 'End' && !search) {event.preventDefault();highlight(visible.length - 1);}
    else if (event.key === 'Enter' || (event.key === ' ' && !search)) {event.preventDefault();const option = visible[active];if (option && !option.disabled) choose(select, trigger, option.value);}
    else if (event.key === 'Escape') {event.preventDefault();event.stopPropagation();close();}
    else if (event.key === 'Tab') close(false);
  });
  // A modal <dialog> makes everything outside it inert and paints above the page,
  // so a list opened from inside a sheet has to live inside that sheet.
  (trigger.closest<HTMLElement>('dialog[open]') || document.body).append(panel);
  place(panel, trigger);
  const outside = (event: PointerEvent) => {if (!panel.contains(event.target as Node) && !trigger.contains(event.target as Node)) close(false);};
  const reposition = (event: Event) => {if (event.target instanceof Node && panel.contains(event.target)) return;if (trigger.isConnected) place(panel, trigger);else close(false);};
  document.addEventListener('pointerdown', outside, true);
  window.addEventListener('resize', reposition);
  window.addEventListener('scroll', reposition, true);
  open = {select, trigger, panel, cleanup: () => {document.removeEventListener('pointerdown', outside, true);window.removeEventListener('resize', reposition);window.removeEventListener('scroll', reposition, true);}};
  trigger.setAttribute('aria-expanded', 'true');
  (search || list.querySelector<HTMLElement>('.app-select-option.active') || panel).focus({preventScroll: true});
  if (!search) panel.tabIndex = -1;
  list.querySelector<HTMLElement>('.active')?.scrollIntoView({block: 'nearest'});
}

function enhance(select: HTMLSelectElement) {
  if (select.dataset[ENHANCED] || select.multiple || select.size > 1) return;
  select.dataset[ENHANCED] = '1';
  const bare = Boolean(select.closest('.model-chip'));
  const trigger = document.createElement('button');
  trigger.type = 'button';
  trigger.className = `app-select${bare ? ' app-select-bare' : ''}`;
  trigger.setAttribute('aria-haspopup', 'listbox');
  trigger.setAttribute('aria-expanded', 'false');
  const label = select.getAttribute('aria-label') || select.closest('label')?.querySelector('span')?.textContent?.trim();
  if (label) trigger.setAttribute('aria-label', label);
  trigger.innerHTML = `<span class="app-select-label"></span>${bare ? '' : icon('chevron')}`;
  select.classList.add('app-select-native');
  select.tabIndex = -1;
  select.setAttribute('aria-hidden', 'true');
  select.after(trigger);
  syncTrigger(select, trigger);
  // A <label> around the select would forward clicks to the hidden native control.
  trigger.addEventListener('click', event => {event.preventDefault();if (open?.trigger === trigger) close();else openPanel(select, trigger);});
  trigger.addEventListener('keydown', event => {if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(event.key)) {event.preventDefault();openPanel(select, trigger);}});
  // The wrapping <label> points at the hidden native control, so a click on the rest of it
  // (chip padding and chevron, a field's caption) has to be passed on by hand.
  select.closest('label')?.addEventListener('click', event => {
    if (trigger.contains(event.target as Node) || trigger.disabled) return;
    event.preventDefault();
    if (bare) trigger.click(); else trigger.focus();
  });
  select.addEventListener('change', () => syncTrigger(select, trigger));
  new MutationObserver(() => syncTrigger(select, trigger)).observe(select, {attributes: true, attributeFilter: ['disabled'], childList: true, subtree: true});
}

/** Enhance every select now and whenever the UI re-renders. */
export function installSelects(root: ParentNode = document) {
  root.querySelectorAll('select').forEach(enhance);
  new MutationObserver(records => {
    for (const record of records) for (const node of record.addedNodes) {
      if (node instanceof HTMLSelectElement) enhance(node);
      else if (node instanceof Element) node.querySelectorAll('select').forEach(enhance);
    }
    if (open && !open.trigger.isConnected) close(false);
  }).observe(document.body, {childList: true, subtree: true});
}
