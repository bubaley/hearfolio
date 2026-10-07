import { getVersion } from '@tauri-apps/api/app';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { openUrl } from '@tauri-apps/plugin-opener';
import { open, save } from '@tauri-apps/plugin-dialog';
import {tr, currentLanguage, type LanguagePreference} from './i18n';

export type RecognitionConfig = {provider:'local'|'openrouter';model:string;mode:'local'|'streaming'|'transcription'};

export type Entry = {
  id: string; name: string; inputPath: string; outputPath: string | null;
  configuration?: RecognitionConfig | null; model: string | null; createdAt: number; sizeBytes?: number | null; durationSeconds?: number | null;
};
export type Progress = { kind: 'download' | 'transcribe' | 'import' | 'storage' | 'update'; stage: string; current: number; total: number; elapsed: number };
export type Settings = {language:LanguagePreference;systemLanguage:'ru'|'en';openrouterUrl:string;hasToken:boolean;storageParent:string;storagePath:string;lastConfiguration:RecognitionConfig};
export type CloudModel = {id:string;name:string;modes:('streaming'|'transcription')[];preferredMode:'streaming'|'transcription'};
export type RuntimePlatform = 'android'|'macos'|'linux'|'windows'|'ios'|'unknown';
export type RuntimeCapabilities={os:RuntimePlatform;mobile:boolean;localRecognition:boolean;customStorage:boolean;nativeAudio:boolean};
export type AudioSelection={path:string;name?:string|null};
export type AndroidRelease = {version:string;url:string;notes?:string;pub_date?:string;sha256?:string};
export const preview = import.meta.env.DEV && !('__TAURI_INTERNALS__' in window);
export const previewPlatform:RuntimePlatform = preview&&new URLSearchParams(window.location.search).get('platform')==='android'?'android':'macos';
export const previewCapabilities:RuntimeCapabilities={os:previewPlatform,mobile:previewPlatform==='android',localRecognition:previewPlatform!=='android',customStorage:previewPlatform!=='android',nativeAudio:true};
let nativePlatform:RuntimeCapabilities|null=preview?previewCapabilities:null;
export async function loadRuntimePlatform():Promise<RuntimeCapabilities> {nativePlatform=await call<RuntimeCapabilities>('get_runtime_platform');return nativePlatform;}
const previewListeners = new Map<string, Set<(payload: unknown) => void>>();
const previewFiles = new Map<string, { file: File; url: string }>();
const previewTexts = new Map<string, string>();
const installedModels = new Set(['whisper-base']);
let previewEntries: Entry[] = [];
let previewSettings:Settings={language:'system',systemLanguage:currentLanguage(),openrouterUrl:'https://openrouter.ai/api/v1',hasToken:false,storageParent:'/Users/you',storagePath:'/Users/you/.hearfolio',lastConfiguration:{provider:'local',model:'whisper-base',mode:'local'}};
if(previewPlatform==='android')previewSettings={...previewSettings,storageParent:'',storagePath:'/data/user/0/app.hearfolio/files/.hearfolio',lastConfiguration:{provider:'openrouter',model:'google/gemini-2.5-flash',mode:'streaming'}};
if (preview) {
  try {
    const saved = JSON.parse(localStorage.getItem('hearfolio-preview-settings') || 'null');
    if (saved) previewSettings = {...previewSettings, ...saved};
    if(previewPlatform==='android'){previewSettings.storageParent='';previewSettings.storagePath='/data/user/0/app.hearfolio/files/.hearfolio';}
    if(previewPlatform==='android'&&previewSettings.lastConfiguration.provider==='local')previewSettings.lastConfiguration={provider:'openrouter',model:'google/gemini-2.5-flash',mode:'streaming'};
  } catch { /* Ignore malformed development preview preferences. */ }
}
function emit(name: string, payload: unknown) { previewListeners.get(name)?.forEach(callback => callback(payload)); }
const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

export async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (!preview) return invoke<T>(command, args);
  let value: unknown;
  switch (command) {
    case 'get_runtime_platform':value={...previewCapabilities};break;
    case 'get_latest_android_release':value=new URLSearchParams(window.location.search).get('update')==='available'?{version:'0.2.0',url:'https://github.com/bubaley/hearfolio/releases/latest'}:null;break;
    case 'runtime_status': value = {ffmpeg:true,whisper:true,nemo:false}; break;
    case 'model_status': value = ['whisper-tiny','whisper-base','whisper-small','nemotron-3.5'].map(id => ({id,installed:installedModels.has(id)})); break;
    case 'list_history': value = previewEntries.map(entry => ({...entry})); break;
    case 'get_settings': value = {...previewSettings}; break;
    case 'save_settings':
      previewSettings = {...previewSettings, ...(args.settings as Partial<Settings>), hasToken: args.token !== undefined ? Boolean(args.token) : previewSettings.hasToken};
      localStorage.setItem('hearfolio-preview-settings', JSON.stringify(previewSettings));
      value = {...previewSettings}; break;
    case 'set_storage_parent':
      for(let current=0;current<=3;current++){emit('task-progress',{kind:'storage',stage:'progress.storage_copy',current,total:3,elapsed:current});await delay(120);}
      previewSettings={...previewSettings,storageParent:String(args.parent),storagePath:String(args.parent).replace(/\/$/,'')+'/.hearfolio'};
      localStorage.setItem('hearfolio-preview-settings',JSON.stringify(previewSettings));value={...previewSettings};break;
    case 'list_openrouter_models':
      await delay(350);
      value=[{id:'google/gemini-2.5-flash',name:'Gemini 2.5 Flash',modes:['streaming'],preferredMode:'streaming'},{id:'google/gemini-2.5-pro',name:'Gemini 2.5 Pro',modes:['streaming'],preferredMode:'streaming'},{id:'fish-audio/transcribe-1-pro',name:'Fish Audio Transcribe 1 Pro',modes:['transcription'],preferredMode:'transcription'}];break;
    case 'update_record_configuration': {
      const entry=previewEntries.find(entry=>entry.id===args.id);if(!entry)throw new Error('errors.record_not_found');
      entry.configuration={...(args.configuration as RecognitionConfig)};value={...entry};break;
    }
    case 'import_audio': {
      const path = String(args.path); const selected = previewFiles.get(path);
      const entry: Entry = {id:crypto.randomUUID(),name:selected?.file.name || (args.name?String(args.name):'') || path.split('/').pop() || 'Recording.m4a',configuration:{...(args.configuration as RecognitionConfig || previewSettings.lastConfiguration)},inputPath:path,outputPath:null,model:null,createdAt:Date.now(),sizeBytes:selected?.file.size};
      previewEntries.unshift(entry); value = {...entry}; break;
    }
    case 'get_history': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (!entry) throw new Error('errors.record_not_found');
      value = {entry:{...entry}, text:previewTexts.get(entry.id) || ''}; break;
    }
    case 'rename_history': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (!entry) throw new Error('errors.record_not_found');
      const name=String(args.name).trim();
      if (!name || name.length>200 || /[\u0000-\u001f\u007f]/.test(name)) throw new Error('errors.invalid_record_name');
      entry.name=name; value={...entry}; break;
    }
    case 'delete_history': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (entry) releasePreviewFile(entry.inputPath);
      previewEntries = previewEntries.filter(entry => entry.id !== args.id); previewTexts.delete(String(args.id)); break;
    }
    case 'clear_history': previewEntries.forEach(entry => releasePreviewFile(entry.inputPath)); previewEntries=[]; previewTexts.clear(); break;
    case 'download_model':
      for (let current = 0; current <= 100; current += 20) { emit('task-progress', {kind:'download',stage:'progress.download',current:current * 1048576,total:100 * 1048576,elapsed:current / 20}); await delay(200); }
      installedModels.add(String(args.id)); break;
    case 'transcribe': {
      const entry = previewEntries.find(entry => entry.id === args.id);
      if (!entry) throw new Error('errors.record_not_found');
      const configuration={...(args.configuration as RecognitionConfig || entry.configuration || previewSettings.lastConfiguration)};
      entry.configuration=configuration;
      const result = currentLanguage()==='en'?'Today we discussed our plans for next week. We will start by updating the interface to make recordings easy to work with.\n\nThe transcript keeps all our decisions together so we can revisit them later. We will assign tasks after the meeting and review the results on Friday.':'Сегодня мы обсудили планы на следующую неделю. Начнём с обновления интерфейса: важно сделать работу с записями простой и понятной.\n\nВсе договорённости сохраняются в расшифровке, чтобы к ним можно было вернуться позже. Задачи распределим после встречи, а в пятницу проверим результат.';
      for (let current = 0; current <= result.length; current += 40) {
        emit('task-progress', {kind:'transcribe',stage:'progress.transcribe',current,total:result.length,elapsed:Math.floor(current / 80)});
        if(current>0){previewSettings.lastConfiguration=configuration;localStorage.setItem('hearfolio-preview-settings',JSON.stringify(previewSettings));}emit('partial-text', {text:result.slice(0,current)}); await delay(180);
      }
      previewTexts.set(entry.id,result); entry.outputPath='preview.txt'; entry.model=configuration.model; value=result; break;
    }
    default: throw new Error(`Preview command unavailable: ${command}`);
  }
  return value as T;
}
function releasePreviewFile(path: string) { const selected=previewFiles.get(path); if (selected) URL.revokeObjectURL(selected.url); previewFiles.delete(path); }
export function registerPreviewFile(file: File) { const path=`preview:${crypto.randomUUID()}`; previewFiles.set(path,{file,url:URL.createObjectURL(file)}); return path; }
export function audioSource(entry: Entry) { return preview ? previewFiles.get(entry.inputPath)?.url || '' : convertFileSrc(entry.inputPath); }
export async function pickAudio(): Promise<AudioSelection | null> {
  if(!preview&&nativePlatform?.mobile&&nativePlatform.nativeAudio)return call<AudioSelection|null>('pick_audio_file');
  if(!preview){const path=await open({multiple:false,filters:[{name:tr('Аудио'),extensions:['m4a','mp3','wav','aac','flac','ogg','mp4']}]});return path?{path}:null;}
  return new Promise(resolve => {
    const input=document.createElement('input'); input.type='file'; input.accept='audio/*,video/mp4';
    input.onchange=()=>resolve(input.files?.[0] ? {path:registerPreviewFile(input.files[0]),name:input.files[0].name} : null);
    input.oncancel=()=>resolve(null); input.click();
  });
}
export async function exportText(name: string, text: string) {
  if(!preview&&nativePlatform?.mobile){await call<boolean>('export_text',{name,content:text});return;}
  if (preview) { const url=URL.createObjectURL(new Blob([text],{type:'text/plain;charset=utf-8'})); const anchor=document.createElement('a'); anchor.href=url; anchor.download=name; anchor.click(); URL.revokeObjectURL(url); return; }
  const path=await save({defaultPath:name,filters:[{name:currentLanguage()==='ru'?'Текст':'Text',extensions:['txt']}]});
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
  if (preview||nativePlatform?.mobile) return Promise.resolve(()=>{});
  return getCurrentWebview().onDragDropEvent(event=>{if(event.payload.type==='drop') callback(event.payload.paths);});
}

export async function openExternal(url:string) { if(preview){window.open(url,'_blank','noopener,noreferrer');return;} await openUrl(url); }

export async function pickStorageParent(defaultPath:string):Promise<string|null> {
  if(!preview)return open({directory:true,multiple:false,defaultPath,title:currentLanguage()==='ru'?'Выберите папку для архива':'Choose archive parent folder'});
  return window.prompt(currentLanguage()==='ru'?'Родительская папка (предпросмотр)':'Parent folder (preview)',defaultPath);
}

export async function appVersion():Promise<string> {return preview?'0.1.0':getVersion();}
export type UpdateProgress = {current:number;total:number;finished:boolean};
export type AppUpdate = {version:string;install:(onProgress:(progress:UpdateProgress)=>void)=>Promise<void>;close:()=>Promise<void>};
export async function checkDesktopUpdate():Promise<AppUpdate|null> {
  if(preview){
    if(new URLSearchParams(window.location.search).get('update')!=='available')return null;
    return {version:'0.2.0',install:async onProgress=>{for(let current=0;current<=5;current++){onProgress({current:current*1048576,total:5*1048576,finished:current===5});await delay(120);}},close:async()=>{}};
  }
  const {check}=await import('@tauri-apps/plugin-updater');
  const update=await check({timeout:15000});if(!update)return null;
  return {version:update.version,install:async onProgress=>{
    let current=0,total=0;
    await update.downloadAndInstall(event=>{
      if(event.event==='Started'){total=event.data.contentLength||0;current=0;}
      if(event.event==='Progress')current+=event.data.chunkLength;
      onProgress({current,total,finished:event.event==='Finished'});
    });
  },close:()=>update.close()};
}
export async function restartApp() {if(preview)return;const {relaunch}=await import('@tauri-apps/plugin-process');await relaunch();}
export async function latestAndroidRelease():Promise<AndroidRelease|null> {return call<AndroidRelease|null>('get_latest_android_release');}
export function isNewerVersion(candidate:string,current:string):boolean {
  const parse=(value:string)=>/^v?(\d+)\.(\d+)\.(\d+)(?:-([\w.-]+))?$/.exec(value);
  const next=parse(candidate),installed=parse(current);if(!next||!installed)return false;
  for(let index=1;index<=3;index++){const difference=Number(next[index])-Number(installed[index]);if(difference!==0)return difference>0;}
  return Boolean(installed[4])&&!next[4];
}
