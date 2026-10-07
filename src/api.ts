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
export type RuntimeCapabilities={os:RuntimePlatform;mobile:boolean;localRecognition:boolean;customStorage:boolean;nativeAudio:boolean;audioRecording:boolean};
export type AudioSelection={path:string;name?:string|null};
export type AndroidRelease = {version:string;url:string;notes?:string;pub_date?:string;sha256?:string};
export const preview = import.meta.env.DEV && !('__TAURI_INTERNALS__' in window);
export const previewPlatform:RuntimePlatform = preview&&new URLSearchParams(window.location.search).get('platform')==='android'?'android':'macos';
export const previewCapabilities:RuntimeCapabilities={os:previewPlatform,mobile:previewPlatform==='android',localRecognition:previewPlatform!=='android',customStorage:previewPlatform!=='android',nativeAudio:true,audioRecording:true};
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
      const entry: Entry = {id:crypto.randomUUID(),name:(args.name?String(args.name):'') || selected?.file.name || path.split('/').pop() || 'Recording.m4a',configuration:{...(args.configuration as RecognitionConfig || previewSettings.lastConfiguration)},inputPath:path,outputPath:null,model:null,createdAt:Date.now(),sizeBytes:selected?.file.size};
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

type BrowserRecording={stream:MediaStream;context:AudioContext;source:MediaStreamAudioSourceNode;processor:ScriptProcessorNode;mute:GainNode;queue:Promise<void>;pending:number;error:Error|null;chunks:Int16Array[]};
let browserRecording:BrowserRecording|null=null;
let recordingGeneration=0, recordingStarting=false, fixtureRecording=false;
let nativeRecordingActive=false;
function microphoneError(error:unknown):Error {
  if(typeof error==='string'&&error.startsWith('errors.'))return new Error(error);
  if(error instanceof Error&&error.message.startsWith('errors.'))return error;
  const name=error instanceof DOMException?error.name:'';
  return new Error(name==='NotAllowedError'||name==='SecurityError'?'errors.microphonePermissionDenied':name==='NotFoundError'||name==='NotSupportedError'?'errors.recordingUnavailable':'errors.recordingStart');
}
function closeBrowserRecording(recording:BrowserRecording) {
  recording.processor.onaudioprocess=null;recording.processor.disconnect();recording.source.disconnect();recording.mute.disconnect();
  recording.stream.getTracks().forEach(track=>track.stop());void recording.context.close().catch(()=>{});
}
function recordingWav(chunks:Int16Array[],rate:number):File {
  const length=chunks.reduce((sum,chunk)=>sum+chunk.length*2,0),header=new ArrayBuffer(44),view=new DataView(header);
  const text=(offset:number,value:string)=>{for(let index=0;index<value.length;index++)view.setUint8(offset+index,value.charCodeAt(index));};
  text(0,'RIFF');view.setUint32(4,length+36,true);text(8,'WAVE');text(12,'fmt ');view.setUint32(16,16,true);view.setUint16(20,1,true);view.setUint16(22,1,true);view.setUint32(24,rate,true);view.setUint32(28,rate*2,true);view.setUint16(32,2,true);view.setUint16(34,16,true);text(36,'data');view.setUint32(40,length,true);
  return new File([header,...chunks.map(chunk=>new Uint8Array(chunk.buffer) as BlobPart)],`Recording-${Date.now()}.wav`,{type:'audio/wav'});
}
export async function startAudioRecording():Promise<void> {
  if(recordingStarting||browserRecording||fixtureRecording||nativeRecordingActive)throw new Error('errors.operationBusy');
  if(!preview&&nativePlatform?.os==='android'){recordingStarting=true;const generation=++recordingGeneration;try{await call('start_audio_recording');if(generation!==recordingGeneration){await call('cancel_audio_recording');throw new Error('errors.recordingUnavailable');}nativeRecordingActive=true;}finally{recordingStarting=false;}return;}
  if(preview&&new URLSearchParams(location.search).get('recording')==='fixture'){fixtureRecording=true;return;}
  if(!navigator.mediaDevices?.getUserMedia||typeof AudioContext==='undefined')throw new Error('errors.recordingUnavailable');
  recordingStarting=true;const generation=++recordingGeneration;
  let stream:MediaStream|null=null,context:AudioContext|null=null;
  try {
    stream=await navigator.mediaDevices.getUserMedia({audio:true,video:false});
    if(generation!==recordingGeneration)throw new Error('errors.recordingUnavailable');
    context=new AudioContext();await context.resume();
    if(generation!==recordingGeneration)throw new Error('errors.recordingUnavailable');
    if(!preview)await call('start_audio_recording',{sampleRate:context.sampleRate});
    if(generation!==recordingGeneration)throw new Error('errors.recordingUnavailable');
    const source=context.createMediaStreamSource(stream),processor=context.createScriptProcessor(4096,1,1),mute=context.createGain();mute.gain.value=0;
    const recording:BrowserRecording={stream,context,source,processor,mute,queue:Promise.resolve(),pending:0,error:null,chunks:[]};
    browserRecording=recording;
    processor.onaudioprocess=event=>{
      if(recording.error)return;
      const input=event.inputBuffer.getChannelData(0),samples=new Int16Array(input.length);
      for(let i=0;i<input.length;i++){const sample=Math.max(-1,Math.min(1,input[i]));samples[i]=Math.round(sample*(sample<0?32768:32767));}
      if(preview){recording.chunks.push(samples);return;}
      // Bound queued IPC chunks; a stalled disk must never buffer an hour in RAM.
      if(recording.pending>=8){recording.error=new Error('errors.recordingStop');recording.stream.getTracks().forEach(track=>track.stop());return;}
      recording.pending++;
      recording.queue=recording.queue.then(()=>call<void>('append_audio_recording',{samples:Array.from(samples)})).catch(()=>{recording.error=new Error('errors.recordingStop');recording.stream.getTracks().forEach(track=>track.stop());}).finally(()=>{recording.pending--;});
    };
    source.connect(processor);processor.connect(mute);mute.connect(context.destination);
  } catch(error) {
    stream?.getTracks().forEach(track=>track.stop());void context?.close().catch(()=>{});
    if(!preview)await call('cancel_audio_recording').catch(()=>{});
    throw microphoneError(error);
  } finally {recordingStarting=false;}
}
export async function stopAudioRecording():Promise<AudioSelection|null> {
  if(!preview&&nativePlatform?.os==='android'){try{return await call<AudioSelection>('stop_audio_recording');}finally{nativeRecordingActive=false;}}
  if(fixtureRecording){fixtureRecording=false;const samples=Int16Array.from({length:1600},(_,index)=>Math.round(Math.sin(index*2*Math.PI*440/16000)*1000));const file=recordingWav([samples],16000);return {path:registerPreviewFile(file),name:file.name};}
  const recording=browserRecording;if(!recording)throw new Error('errors.recordingUnavailable');browserRecording=null;
  closeBrowserRecording(recording);await recording.queue;
  if(recording.error){if(!preview)await call('cancel_audio_recording').catch(()=>{});throw recording.error;}
  if(!preview)return call<AudioSelection>('stop_audio_recording');
  if(!recording.chunks.length)throw new Error('errors.recordingEmpty');
  const file=recordingWav(recording.chunks,recording.context.sampleRate);return {path:registerPreviewFile(file),name:file.name};
}
export async function cancelAudioRecording():Promise<void> {
  recordingGeneration++;fixtureRecording=false;
  nativeRecordingActive=false;
  const recording=browserRecording;browserRecording=null;
  if(recording){closeBrowserRecording(recording);await recording.queue;}
  if(!preview)await call('cancel_audio_recording');
}
export async function discardAudioRecording(selection:AudioSelection):Promise<void> {
  if(preview){releasePreviewFile(selection.path);return;}
  await call('discard_audio_recording',{path:selection.path});
}
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
