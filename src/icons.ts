const paths: Record<string, string> = {
  wave: 'M3 10v4m4-7v10m5-14v18m5-14v10m4-7v4',
  file: 'M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9l-6-6zm0 0v6h6M8 14h8m-8 4h5',
  history: 'M3 12a9 9 0 1 0 3-7M3 3v6h6m3-2v5l3 2',
  models: 'm12 3 9 5-9 5-9-5 9-5zm-9 9 9 5 9-5M3 16l9 5 9-5',
  settings: 'M4 7h16M4 17h16M8 4v6m8 4v6',
  sun: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8m0-6v2m0 16v2M2 12h2m16 0h2M5 5l1 1m12 12 1 1M5 19l1-1M18 6l1-1',
  moon: 'M20 15.5A8.5 8.5 0 0 1 8.5 4 8.5 8.5 0 1 0 20 15.5',
  plus: 'M12 5v14M5 12h14',
  search: 'M21 21l-5-5M10.5 3a7.5 7.5 0 1 0 0 15 7.5 7.5 0 0 0 0-15',
  upload: 'M12 16V4m-4 4 4-4 4 4M4 16v4h16v-4',
  copy: 'M9 9h11v11H9zM15 9V4H4v11h5',
  edit: 'm15 4 5 5M4 20l4-1L20 7a2.8 2.8 0 0 0-4-4L4 15l-1 6 5-1',
  arrow: 'M12 19V5m-6 6 6-6 6 6',
  check: 'm5 12 4 4L19 6',
  trash: 'M4 6h16M9 6V3h6v3M6 6l1 15h10l1-15M10 10v7m4-7v7',
  download: 'M12 3v12m-4-4 4 4 4-4M4 17v4h16v-4',
  panel: 'M3 4h18v16H3zM9 4v16',
  close: 'm6 6 12 12M6 18 18 6',
  chevron: 'm9 5 7 7-7 7',
  alert: 'm12 3 10 18H2L12 3zm0 6v5m0 3h.01',
  headphones: 'M4 14v-3a8 8 0 0 1 16 0v3M4 12h3v8H4v-8zm13 0h3v8h-3v-8',
  cloud: 'M6 18a5 5 0 0 1-1-10 7 7 0 0 1 13-1 5.5 5.5 0 0 1 0 11H6',
  share: 'M12 16V3m-4 4 4-4 4 4M5 12v9h14v-9',
  pause: 'M8 5v14M16 5v14',
  play: 'm9 5 11 7-11 7V5',
};
export function icon(name: string): string {
  return `<svg viewBox="0 0 24 24" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.65" stroke-linecap="round" stroke-linejoin="round"><path d="${paths[name] || paths.file}"/></svg>`;
}
export const escapeHtml = (value: string) => value.replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]!));
