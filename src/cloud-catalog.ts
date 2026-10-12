export type CatalogModel = {
  id: string;
  name: string;
  modes: ('streaming' | 'transcription')[];
  preferredMode: 'streaming' | 'transcription';
};

type CatalogStorage = Pick<Storage, 'getItem' | 'setItem'>;
const cacheKey = 'hearfolio-audio-catalog-v1';
const catalogUrl = (url: string) => url.trim().replace(/\/+$/, '');

export function normalizeAudioCatalog(value: unknown): CatalogModel[] {
  if (!Array.isArray(value)) return [];
  const models = new Map<string, CatalogModel>();
  for (const item of value) {
    if (!item || typeof item.id !== 'string' || !item.id.trim() || typeof item.name !== 'string' || !Array.isArray(item.modes)) continue;
    const modes = [...new Set(item.modes.filter((mode: unknown) => mode === 'streaming' || mode === 'transcription'))] as CatalogModel['modes'];
    if (!modes.length) continue;
    models.set(item.id, {id: item.id, name: item.name, modes, preferredMode: modes.includes('transcription') ? 'transcription' : 'streaming'});
  }
  return [...models.values()];
}

// Catalog metadata contains no token. Requests still validate the model on the backend.
export function readAudioCatalog(storage: CatalogStorage, url: string): CatalogModel[] {
  try {
    const saved = JSON.parse(storage.getItem(cacheKey) || 'null');
    return saved?.url === catalogUrl(url) ? normalizeAudioCatalog(saved.models) : [];
  } catch { return []; }
}

export function saveAudioCatalog(storage: CatalogStorage, url: string, models: CatalogModel[]): void {
  try { storage.setItem(cacheKey, JSON.stringify({url: catalogUrl(url), models: normalizeAudioCatalog(models)})); }
  catch { /* Recognition works even when WebView storage is unavailable. */ }
}
