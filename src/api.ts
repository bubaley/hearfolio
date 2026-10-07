import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { openUrl } from '@tauri-apps/plugin-opener';
import { open, save } from '@tauri-apps/plugin-dialog';

export type Entry = {
  id: string; name: string; inputPath: string; outputPath: string | null;
  model: string | null; createdAt: number; sizeBytes?: number | null; durationSeconds?: number | null;
};
export type Progress = { kind: 'download' | 'transcribe' | 'import'; stage: string; current: number; total: number; elapsed: number };
export type Settings = { provider: 'local' | 'openrouter'; localModel: string; openrouterUrl: string; hasToken: boolean; openrouterMode: 'streaming' | 'transcription'; openrouterModel: string };
export type CloudModel = { id: string; name: string; inputModalities?: string[] };
export const preview = import.meta.env.DEV && !('__TAURI_INTERNALS__' in window);
const previewListeners = new Map<string, Set<(payload: unknown) => void>>();
const previewFiles = new Map<string, { file: File; url: string }>();
const previewTexts = new Map<string, string>();
const installedModels = new Set(['whisper-base']);
let previewEntries: Entry[] = [];
let previewSettings: Settings = {provider:'local',localModel:'whisper-base',openrouterUrl:'https://openrouter.ai/api/v1',hasToken:false,openrouterMode:'streaming',openrouterModel:'google/gemini-2.5-flash'};
if (preview) {
  try {
    const saved = JSON.parse(localStorage.getItem('hearfolio-preview-settings') || 'null');
    if (saved) previewSettings = {...previewSettings, ...saved};
  } catch { /* Ignore malformed development preview preferences. */ }
}
function emit(name: string, payload: unknown) { previewListeners.get(name)?.forEach(callback => callback(payload)); }
const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

export async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!preview) return invoke<T>(command, args);
  let value: unknown;
  switch (command) {
    case 'runtime_status': value = {ffmpeg:true,whisper:true,nemo:false}; break;
    case 'model_status': value = ['whisper-tiny','whisper-base','whisper-small','nemotron-3.5'].map(id => ({id,installed:installedModels.has(id)})); break;
    case 'list_history': value = previewEntries.map(entry => ({...entry})); break;
    case 'get_settings': value = {...previewSettings}; break;
    case 'save_settings':
      previewSettings = {...previewSettings, ...(args.settings as Settings), hasToken: args.token !== undefined ? Boolean(args.token) : previewSettings.hasToken};
      localStorage.setItem('hearfolio-preview-settings', JSON.stringify(previewSettings));
      value = {...previewSettings}; break;
    case 'list_openrouter_models': value = [{id:'google/gemini-2.5-flash',name:'Gemini 2.5 Flash',inputModalities:['audio','text']}]; break;
    case 'import_audio': {
      const path = String(args.path); const selected = previewFiles.get(path);
      const entry: Entry = {id:crypto.randomUUID(),name:selected?.file.name || path.split('/').pop() || 'Запись.m4a',inputPath:path,outputPath:null,model:null,createdAt:Date.now(),sizeBytes:selected?.file.size};
      previewEntries.unshift(entry); value = {...entry}; break;
    }
    case 'get_history': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (!entry) throw new Error('Запись не найдена');
      value = {entry:{...entry}, text:previewTexts.get(entry.id) || ''}; break;
    }
    case 'rename_history': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (!entry) throw new Error('Запись не найдена');
      const name=String(args.name).trim();
      if (!name || name.length>200 || /[\u0000-\u001f\u007f]/.test(name)) throw new Error('Введите название длиной от 1 до 200 символов');
      entry.name=name; value={...entry}; break;
    }
    case 'delete_history': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (entry) releasePreviewFile(entry.inputPath);
      previewEntries = previewEntries.filter(entry => entry.id !== args.id); previewTexts.delete(String(args.id)); break;
    }
    case 'clear_history': previewEntries.forEach(entry => releasePreviewFile(entry.inputPath)); previewEntries=[]; previewTexts.clear(); break;
    case 'download_model':
      for (let current = 0; current <= 100; current += 20) { emit('task-progress', {kind:'download',stage:'Скачиваем модель',current:current * 1048576,total:100 * 1048576,elapsed:current / 20}); await delay(200); }
      installedModels.add(String(args.id)); break;
    case 'transcribe': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (!entry) throw new Error('Запись не найдена');
      const result = 'Сегодня мы обсудили планы на следующую неделю. Начнём с обновления интерфейса: важно сделать работу с записями простой и понятной.\n\nВсе договорённости сохраняются в расшифровке, чтобы к ним можно было вернуться позже. Задачи распределим после встречи, а в пятницу проверим результат.';
      for (let current = 0; current <= result.length; current += 40) {
        emit('task-progress', {kind:'transcribe',stage:'Распознаём запись',current,total:result.length,elapsed:Math.floor(current / 80)});
        emit('partial-text', {text:result.slice(0,current)}); await delay(180);
      }
      previewTexts.set(entry.id,result); entry.outputPath='preview.txt'; entry.model=String(args.model); value=result; break;
    }
    default: throw new Error(`Команда недоступна в предпросмотре: ${command}`);
  }
  return value as T;
}
function releasePreviewFile(path: string) { const selected=previewFiles.get(path); if (selected) URL.revokeObjectURL(selected.url); previewFiles.delete(path); }
export function registerPreviewFile(file: File) { const path=`preview:${crypto.randomUUID()}`; previewFiles.set(path,{file,url:URL.createObjectURL(file)}); return path; }
export function audioSource(entry: Entry) { return preview ? previewFiles.get(entry.inputPath)?.url || '' : convertFileSrc(entry.inputPath); }
export async function pickAudio(): Promise<string | null> {
  if (!preview) return open({multiple:false,filters:[{name:'Аудио',extensions:['m4a','mp3','wav','aac','flac','ogg','mp4']}]});
  return new Promise(resolve => {
    const input=document.createElement('input'); input.type='file'; input.accept='audio/*,video/mp4';
    input.onchange=()=>resolve(input.files?.[0] ? registerPreviewFile(input.files[0]) : null);
    input.oncancel=()=>resolve(null); input.click();
  });
}
export async function exportText(name: string, text: string) {
  if (preview) { const url=URL.createObjectURL(new Blob([text],{type:'text/plain;charset=utf-8'})); const anchor=document.createElement('a'); anchor.href=url; anchor.download=name; anchor.click(); URL.revokeObjectURL(url); return; }
  const path=await save({defaultPath:name,filters:[{name:'Текст',extensions:['txt']}]});
  if (path) await call('save_text',{path,content:text});
}
export function subscribe<T>(name: string, callback: (payload: T) => void) {
  if (!preview) return listen<T>(name,event=>callback(event.payload));
  const listener=(payload:unknown)=>callback(payload as T);
  if (!previewListeners.has(name)) previewListeners.set(name,new Set());
  previewListeners.get(name)!.add(listener);
  return Promise.resolve(()=>previewListeners.get(name)?.delete(listener));
}
export function subscribeAudioDrop(callback: (paths: string[]) => void) {
  if (preview) return Promise.resolve(()=>{});
  return getCurrentWebview().onDragDropEvent(event=>{if(event.payload.type==='drop') callback(event.payload.paths);});
}

export async function openExternal(url:string) { if(preview){window.open(url,'_blank','noopener,noreferrer');return;} await openUrl(url); }
