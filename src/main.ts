import {recordNameParts, renamedRecordTitle} from './record-name';
import {normalizeAudioCatalog, readAudioCatalog, saveAudioCatalog} from './cloud-catalog';
import {RecorderEnvelope,NativeRecordingOrder,NativeRecorderClock,RecordingSnapshotPoll} from './recorder-meter';
import {drawRecorder,readPalette,type RecorderPalette} from './recorder-canvas';
import {Pacer} from './pacer';
import {pauseAudioRecording, resumeAudioRecording, restoreNativeRecordingFlag, observePendingOperation, operationHandled, markOperationHandled} from './api';
import type {OperationSnapshot} from './operation-recovery';
import {transferSettingsHtml, devicesHtml, transferBarHtml, shareRecordingToDevice, bindTransfers, initializeTransfers, hasTransferDraft, discardTransferDraft, transferActivityLabel} from './transfers';
import './styles.css';
import './transfers.css';
import {renderSettings, initialSettingsDraft, mergeSettingsDraft, hasSettingsChanges, isSettingsSection, settingsLabel, type SettingsSection, type SettingsDraft} from './settings-ui';
import {recordingInputLevel, call, pickAudio, startAudioRecording, stopAudioRecording, cancelAudioRecording, discardAudioRecording, exportText, subscribe, subscribeAudioDrop, registerPreviewFile, audioSource, shareAudio, shareText, saveAudio, prepareNativePlayback, nativePlaybackState, playNativePlayback, pauseNativePlayback, seekNativePlayback, releaseNativePlayback, type NativePlayerState, type PartialTextEvent, type CloudResume, openExternal, openAndroidBatterySettings, pickStorageParent, loadRuntimePlatform, previewCapabilities, appVersion, checkDesktopUpdate, latestAndroidRelease, restartApp, isNewerVersion, preview, listLooseRecordings, adoptLooseRecording, exportLooseRecording, type LooseRecording, type AudioSelection, type Entry, type Progress, type Settings, type CloudModel, type RecognitionConfig, type RuntimeCapabilities, type AndroidRelease, type AppUpdate} from './api';
import {icon, escapeHtml as esc} from './icons';
import {tr, locale, countLabel, errorText, setLanguage, currentLanguage, type LanguagePreference} from './i18n';

import {setupPostprocess, rulesHtml, postprocessHtml, resultsHtml, textPickerHtml, textPickerOpen, bindPostprocess, ensureRules, hasRuleDraft, mayLeaveRules, restorePostprocessActivity} from './postprocess';

type View = 'work' | 'history' | 'archive' | 'models' | 'settings' | 'rules' | 'devices';
let settingsSection:SettingsSection|null=null, settingsSaving=false, settingsError='';
const models = [
  {id:'whisper-tiny',name:'Whisper Tiny',detail:"Для коротких заметок. Быстрее остальных, но чаще ошибается.",size:"75 МБ",installed:false},
  {id:'whisper-base',name:'Whisper Base',detail:"Для повседневных записей. Хороший выбор для начала.",size:"142 МБ",installed:false},
  {id:'whisper-small',name:'Whisper Small',detail:"Для встреч и сложной речи. Точнее, требует больше времени.",size:"466 МБ",installed:false},
  {id:'nemotron-3.5',name:'Nemotron 3.5',detail:"Многоязычное распознавание. Нужен отдельный движок NeMo.",size:"708 МБ",installed:false},
];
const app = document.querySelector<HTMLDivElement>('#app')!;
let view:View='work', history:Entry[]=[], active:Entry|null=null, text='', busy=false, loading=true;
let status='', isError=false, progress:Progress|null=null, resultSaved=false, search='', cloudLoading=false;
let settings:Settings={language:'system',systemLanguage:currentLanguage(),openrouterUrl:'https://openrouter.ai/api/v1',chunkSeconds:120,hasToken:false,storageParent:'',storagePath:'',lastConfiguration:{provider:'local',model:'whisper-base',mode:'local'}};
let platform:RuntimeCapabilities=preview?{...previewCapabilities}:{os:'unknown',mobile:false,localRecognition:false,customStorage:false,nativeAudio:false,audioRecording:false};
let applicationVersion='',updateChecking=false,updateChecked=false,updateError='',pendingUpdate:AppUpdate|null=null,androidRelease:AndroidRelease|null=null,updateInstalled=false,installedUpdateVersion='',lastNotifiedUpdate='',lastUpdateCheck=0;
let configuration:RecognitionConfig={...settings.lastConfiguration};
let localChoice='whisper-base',cloudChoice='',catalogOpen=false,catalogSearch='',catalogError='';
let configurationSaveQueue:Promise<unknown>=Promise.resolve();
let settingsDraft:SettingsDraft|null=null, cloudModels:CloudModel[]=[];
let runtime={ffmpeg:false,whisper:false,nemo:false};
let runtimeLoaded=false, historyError='', sidebarCollapsed=localStorage.getItem('hearfolio-sidebar')==='collapsed'||(!localStorage.getItem('hearfolio-sidebar')&&window.innerWidth<=760);
let theme=localStorage.getItem('hearing-theme')||(matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light');
let operationStarted=0, elapsedTimer:ReturnType<typeof setInterval>|null=null;
let nativeOperation:OperationSnapshot|null=null, operationPolling=false, operationCancelling=false, recoveringOperation=false;
let settingsSaveQueue:Promise<unknown>=Promise.resolve(), previousResult:{text:string;saved:boolean}|null=null;
let receivedPartial=false, audioPosition=0, audioWasPlaying=false, pickingAudio=false;
let noticeTimer:ReturnType<typeof setTimeout>|null=null;
let recordingPhase:'idle'|'starting'|'recording'|'paused'|'stopping'|'cancelling'='idle', recordingStarted=0, recordingTimer:ReturnType<typeof setInterval>|null=null, pendingRecording:AudioSelection|null=null;
let outputTab:'transcript'|'results'='transcript';
// Captured audio whose import never finished; the archive view lists it for retry or export.
let loose:LooseRecording[]=[],looseLoading=false,looseError='';
let resume:(CloudResume&{id:string})|null=null;
async function refreshResume(id:string){try{const state=await call<CloudResume|null>('cloud_resume_state',{id});if(active?.id!==id)return;resume=state&&state.done>0&&state.done<state.total?{id,...state}:null;}catch{resume=null;}if(!busy&&view==='work')render();}
const canResume=()=>Boolean(resume&&active&&resume.id===active.id&&resume.configuration.provider===configuration.provider&&resume.configuration.model===configuration.model&&resume.configuration.mode===configuration.mode);
let playerId='', playerLoading=false, playerError='', playerState:NativePlayerState={duration:0,currentTime:0,playing:false}, playerPoll:ReturnType<typeof setTimeout>|null=null, playerGeneration=0;
let nativeRecordingSeconds:number|null=null,nativeRecordingMillis:number|null=null,nativeRecordingPath='',externalRecording=false,recordingControlPending=false,nativeCaptureGeneration=0;
const recorderEnvelope=new RecorderEnvelope();
const nativeRecordingOrder=new NativeRecordingOrder();
const nativeRecorderClock=new NativeRecorderClock();
const recordingSnapshotPoll=new RecordingSnapshotPoll();
type NativeCapture={state:string;stateRevision?:number;path?:string;elapsedSeconds?:number;elapsedMillis?:number;level?:number;envelope?:number[];envelopeSequence?:number;envelopeIntervalMs?:number};
function recordingMilliseconds(){return nativeRecordingMillis!==null?nativeRecorderClock.milliseconds(performance.now()):(nativeRecordingSeconds!==null?nativeRecordingSeconds*1000:recordingStarted?Math.max(0,(recordingPausedAt||Date.now())-recordingStarted-recordingPausedMillis):0);}
let recorderPalette:RecorderPalette|null=null;
function recordingMeterHtml(){return `<div class="recorder-timeline" role="img" aria-label="${tr('Уровень микрофона')}"><canvas class="recorder-canvas" aria-hidden="true"></canvas></div>`;}
function paintRecorderMeter(){
  const millis=recordingMilliseconds(),canvas=document.querySelector<HTMLCanvasElement>('.recorder-canvas');
  if(canvas){recorderPalette??=readPalette(canvas);drawRecorder(canvas,recorderEnvelope.levels(),recorderEnvelope.newestMillis(),recorderEnvelope.interval,millis,recordingPhase!=='recording',recorderPalette);}
  const timer=document.querySelector('#recording-duration'),label=recordingDuration(millis);if(timer&&timer.textContent!==label)timer.textContent=label;
}
function applyNativeCapture(p:NativeCapture){
  const sameSession=!p.path||p.path===nativeRecordingPath;
  const old=Boolean(['recording','paused'].includes(p.state)&&sameSession&&p.elapsedMillis!==undefined&&nativeRecordingMillis!==null&&p.elapsedMillis<nativeRecordingMillis);
  if(old||!nativeRecordingOrder.accept(p))return false;
  if(p.path&&p.path!==nativeRecordingPath){nativeRecordingPath=p.path;nativeRecordingMillis=null;nativeRecordingSeconds=null;nativeRecorderClock.reset();recorderEnvelope.reset();}
  if(p.elapsedMillis!==undefined){nativeRecordingMillis=p.elapsedMillis;nativeRecorderClock.synchronize(p.elapsedMillis,p.state,performance.now());}
  if(p.elapsedSeconds!==undefined)nativeRecordingSeconds=p.elapsedSeconds;
  if(p.envelope&&p.envelopeSequence!==undefined&&p.path)recorderEnvelope.synchronize(p.envelope,p.envelopeSequence,p.path,p.envelopeIntervalMs||recorderEnvelope.interval);
  requestFrames();return true;
}
// Desktop levels come from the WebView microphone; native capture delivers its own envelope.
function sampleRecorderLevel(){
  if(document.hidden||recordingPhase!=='recording'||(platform.os==='android'&&!preview))return;
  recorderEnvelope.sample(recordingInputLevel(),performance.now(),true,recordingMilliseconds());
}
setInterval(sampleRecorderLevel,recorderEnvelope.interval);

// One animation loop paints everything that moves: the recorder, revealed text and progress.
// A frame requested while the WebView was in the background may never arrive; ask again after a while.
let frameRequestedAt=-1;
function requestFrames(){const now=performance.now();if(frameRequestedAt>=0&&now-frameRequestedAt<500)return;frameRequestedAt=now;requestAnimationFrame(animate);}
function animate(now:number){
  frameRequestedAt=-1;let again=false;
  if(['recording','paused','stopping'].includes(recordingPhase)){paintRecorderMeter();again=recordingPhase!=='stopping';}
  if(revealing){paintReveal(now);again||=revealing;}
  if(busy&&progress?.total){paintProgress(now);again=true;}
  if(again&&!document.hidden)requestFrames();
}
document.addEventListener('visibilitychange',()=>{if(document.hidden)return;if(revealing)textPacer.hurry(900);requestFrames();});

// Text arrives a segment at a time; reveal it at the pace segments arrive so it flows without pauses.
const textPacer=new Pacer('reveal');
let revealing=false,revealRecord='';
function shownText():string {
  if(!revealing||revealRecord!==active?.id)return text;
  let end=Math.min(text.length,Math.floor(textPacer.value));
  const code=text.charCodeAt(end-1);if(code>=0xd800&&code<0xdc00)end--;
  return text.slice(0,end);
}
function startReveal(from=0){textPacer.reset(from,performance.now());revealing=true;revealRecord=active?.id||'';requestFrames();}
function feedReveal(){if(!revealing)return;textPacer.set(text.length,performance.now());requestFrames();}
function finishReveal(){if(!revealing)return;const now=performance.now();textPacer.set(text.length,now);textPacer.finish(now);requestFrames();}
function stopReveal(){revealing=false;}
function paintReveal(now:number){
  if(revealRecord!==active?.id){revealing=false;return;}
  const before=Math.floor(textPacer.value);textPacer.frame(now);
  if(textPacer.finished)revealing=false;
  if(Math.floor(textPacer.value)===before&&revealing)return;
  const article=document.querySelector('#transcript-text'),result=document.querySelector<HTMLElement>('#result');
  if(!article){if(shownText())paintPartial();return;}
  const follow=Boolean(result&&result.scrollHeight-result.scrollTop-result.clientHeight<90);
  article.textContent=shownText();if(follow&&result)result.scrollTop=result.scrollHeight;
}

// Progress arrives in steps (segment i of n); predict motion inside the current step without claiming it.
const progressPacer=new Pacer('predict');
let progressKey='';
function feedProgress(){
  if(!progress?.total)return;
  const key=`${progress.kind}:${progress.total}`,percent=Math.min(100,progress.current/progress.total*100),now=performance.now();
  // Few large steps (segments) are predicted; byte counts are already fine-grained.
  const step=progress.total<=1000?100/progress.total:undefined;
  if(key!==progressKey){progressKey=key;progressPacer.reset(percent,now);if(progress.kind==='transcribe')progressPacer.expect((settings.chunkSeconds||120)*350);}
  progressPacer.set(percent,now,step);requestFrames();
}
function smoothPercent():number|null {return progress?.total?Math.min(99,progressPacer.value):null;}
function paintProgress(now:number){
  progressPacer.frame(now);
  const value=smoothPercent();if(value===null)return;
  const whole=`${Math.floor(value)}%`;
  document.querySelectorAll<HTMLElement>('#progress .track i').forEach(bar=>{bar.style.width=`${value.toFixed(2)}%`;});
  document.querySelectorAll('#progress .progress-percent').forEach(label=>{if(label.textContent!==whole)label.textContent=whole;});
  document.querySelector<HTMLElement>('.stage-wave')?.style.setProperty('--progress',`${value.toFixed(2)}%`);
  document.querySelectorAll('.record-state.live').forEach(chip=>{const next=`<i aria-hidden="true"></i>${whole}`;if(chip.innerHTML!==next)chip.innerHTML=next;});
  const banner=document.querySelector('.job-banner small'),stage=progress?`${progress.stage} · ${whole}`:'';if(banner&&banner.textContent!==stage)banner.textContent=stage;
}
let recordingPausedAt=0,recordingPausedMillis=0;
const recordingDuration=(millis=recordingMilliseconds())=>{return `${String(Math.floor(millis/60000)).padStart(2,'0')}:${String(Math.floor(millis/1000)%60).padStart(2,'0')}.${String(Math.floor(millis/10)%100).padStart(2,'0')}`;};
document.documentElement.dataset.theme=theme;
const title:Record<View,string>={work:"Запись",history:"Записи",archive:"Архив",models:"Модели",settings:"Настройки",devices:"Устройства",rules:"Правила обработки"};
const formatTime=(seconds:number)=>{const whole=Math.floor(seconds),pad=(value:number)=>String(value).padStart(2,'0');return whole>=3600?`${Math.floor(whole/3600)}:${pad(Math.floor(whole/60)%60)}:${pad(whole%60)}`:`${Math.floor(whole/60)}:${pad(whole%60)}`;};
const dateLabel=(timestamp:number)=>new Date(timestamp).toLocaleDateString(locale(),{day:'numeric',month:'long'});
const audioFormats='MP3, M4A, WAV, AAC, FLAC, OGG, MP4';
const recordCount=(count:number)=>countLabel(count,'record');
const disabled=()=>busy||platform.os==='unknown'?'disabled':'';
const deviceSystemLanguage=()=>((navigator.languages?.[0]||navigator.language).toLowerCase().startsWith('ru')?'ru':'en') as 'ru'|'en';
const applyLanguagePreference=()=>setLanguage(settings.language,platform.mobile?deviceSystemLanguage():settings.systemLanguage);
const words=()=>text.trim()?text.trim().split(/\s+/).length:0;
function readiness():string {
  if(platform.os==='unknown')return tr(runtimeLoaded?'Не удалось определить платформу. Перезапустите приложение.':'Проверяем готовность…');
  if (!runtimeLoaded) return tr("Проверяем готовность…");
  if (!runtime.ffmpeg&&(configuration.provider==='local'||!platform.nativeAudio)) return tr("Установите FFmpeg для подготовки аудио.");
  if (configuration.provider==='openrouter') {
    if(!settings.hasToken)return tr("Добавьте API token OpenRouter.");
    if(cloudModels.some(model=>model.id===configuration.model&&model.modes.includes(configuration.mode as 'streaming'|'transcription')))return '';
    return cloudLoading?tr("Загружаем модели…"):catalogError?tr("Обновите каталог моделей."):tr("Выберите доступную модель OpenRouter.");
  }
  const chosen=models.find(model=>model.id===localChoice);
  if (!chosen?.installed) return tr("Скачайте выбранную модель.");
  if (localChoice==='nemotron-3.5'&&!runtime.nemo) return tr("Установите движок NeMo для этой модели.");
  if (localChoice!=='nemotron-3.5'&&!runtime.whisper) return tr("Установите Whisper для локального распознавания.");
  return '';
}
function notice(message:string,error=false) {
  status=message; isError=error;
  if (noticeTimer) clearTimeout(noticeTimer);
  updateNotice();
  if (message&&!error) noticeTimer=setTimeout(()=>{if(status===message&&!isError){status='';updateNotice();}noticeTimer=null;},3500);
}
function updateNotice() {
  const element=document.querySelector<HTMLElement>('#status');
  if (element) {element.className=`notice ${isError?'error':''}`;element.innerHTML=status?`<span>${isError?icon('alert'):icon('check')}${esc(status)}${tr("</span><button class=\"icon-button\" data-dismiss aria-label=\"Закрыть сообщение\">")}${icon('close')}</button>`:'';element.querySelector('[data-dismiss]')?.addEventListener('click',()=>{status='';updateNotice();});}
}
function beginOperation(kind:Progress['kind'],stage:string) {
  if(noticeTimer){clearTimeout(noticeTimer);noticeTimer=null;}
  busy=true;status='';operationStarted=Date.now();progress={kind,stage,current:0,total:0,elapsed:0};progressKey='';progressPacer.reset(0,performance.now());
  if (elapsedTimer) clearInterval(elapsedTimer);
  elapsedTimer=setInterval(()=>{if(progress){progress.elapsed=Math.max(progress.elapsed,Math.floor((Date.now()-operationStarted)/1000));updateProgress();}},1000);
}
function endOperation() {busy=false;progress=null;nativeOperation=null;operationCancelling=false;if(elapsedTimer)clearInterval(elapsedTimer);elapsedTimer=null;}
function progressBodyHtml():string {
  const p=progress!,smooth=smoothPercent(),percent=smooth===null?null:Math.floor(smooth);
  const remaining=p.elapsed>0&&p.current>0&&p.total>p.current?Math.ceil((p.total-p.current)/(p.current/p.elapsed)):0;
  const amount=(p.kind==='download'||p.kind==='update')&&p.total?`${(p.current/1048576).toFixed(1)} / ${(p.total/1048576).toFixed(1)}${tr(" МБ · ")}`:'';
  return `<div class="progress-label"><strong>${esc(p.stage)}</strong><span class="progress-percent">${percent===null?'':`${percent}%`}</span></div><div class="track ${percent===null?'indeterminate':''}"><i style="width:${smooth===null?25:smooth.toFixed(2)}%"></i></div><div class="progress-detail"><span>${amount}${tr("Прошло ")}${formatTime(p.elapsed)}</span><span>${remaining?`${tr("Осталось около ")}${formatTime(remaining)}`:''}</span></div>`;
}
const cancelKey=()=>`${nativeOperation?.state||''}:${operationCancelling}`;
function cancelHtml():string {return nativeOperation?.state==='running'?`<button type="button" class="secondary" id="cancel-operation" ${operationCancelling?'disabled':''}>${tr(operationCancelling?'Отменяем…':'Отменить')}</button>`:'';}
function progressHtml():string {
  if (!busy||!progress) return '';
  return `<div class="task-progress" role="status"><span class="spinner" aria-hidden="true"></span><div class="progress-body" id="progress-body">${progressBodyHtml()}</div><div class="progress-cancel" id="progress-cancel" data-key="${cancelKey()}">${cancelHtml()}</div></div>`;
}
// Patch progress in place so the cancel button keeps focus and never misses a click between ticks.
function updateProgress() {
  const element=document.querySelector('#progress'),body=element?.querySelector('#progress-body'),cancel=element?.querySelector<HTMLElement>('#progress-cancel');
  if(element&&body&&cancel&&busy&&progress){body.innerHTML=progressBodyHtml();if(cancel.dataset.key!==cancelKey()){cancel.innerHTML=cancelHtml();cancel.dataset.key=cancelKey();bindOperationCancel();}}
  else if(element){element.innerHTML=progressHtml();bindOperationCancel();}
  const percent=progressPercent(),wave=document.querySelector<HTMLElement>('.stage-wave');
  if(wave){wave.style.setProperty('--progress',`${percent??0}%`);wave.classList.toggle('indeterminate',busy&&progress?.kind==='transcribe'&&percent===null);}
  const label=percent===null?tr(progress?.kind==='postprocess'?'Обработка':'Распознаём'):`${percent}%`;
  document.querySelectorAll('.record-state.live').forEach(chip=>{chip.innerHTML=`<i aria-hidden="true"></i>${label}`;});
  const banner=document.querySelector('.job-banner small');if(banner&&progress)banner.textContent=`${progress.stage}${percent!==null?` · ${percent}%`:''}`;
}
function bindOperationCancel(){document.querySelector('#cancel-operation')?.addEventListener('click',()=>{void cancelNativeOperation();});}
async function cancelNativeOperation(){if(!nativeOperation?.id||operationCancelling)return;operationCancelling=true;updateProgress();try{await call('cancel_background_operation',{id:nativeOperation.id});}catch(error){operationCancelling=false;notice(errorText(error),true);updateProgress();}}
function groupLabel(timestamp:number) {
  const day=new Date(timestamp);day.setHours(0,0,0,0);const today=new Date();today.setHours(0,0,0,0);
  const age=Math.round((today.getTime()-day.getTime())/86400000);
  return age===0?tr("Сегодня"):age===1?tr("Вчера"):age<7?tr("Предыдущие 7 дней"):age<30?tr("Предыдущие 30 дней"):tr("Ранее");
}
const displayName=(entry:Entry)=>recordNameParts(entry.name,entry.inputPath).title||entry.name;
const sizeLabel=(bytes:number)=>`${(bytes/1048576).toFixed(bytes<10485760?1:0)}${tr(" МБ")}`;
const progressPercent=()=>{const value=smoothPercent();return value===null?null:Math.floor(value);};
const jobRecord=()=>busy&&progress&&(progress.kind==='transcribe'||progress.kind==='postprocess')?active?.id||null:null;
// Deterministic bar heights give each recording a recognisable silhouette in the brand's five-bar style.
function waveBars(seed:string,count:number):number[] {
  let hash=2166136261;for(const char of seed){hash^=char.charCodeAt(0);hash=Math.imul(hash,16777619);}
  return Array.from({length:count},(_,index)=>{hash^=hash<<13;hash^=hash>>>17;hash^=hash<<5;const envelope=.35+.65*Math.sin(Math.PI*(index+.5)/count);return Math.max(.12,Math.min(1,envelope*(.35+((hash>>>0)%1000)/1000*.65)));});
}
function recordStateHtml(entry:Entry):string {
  if(jobRecord()===entry.id){const percent=progressPercent();return `<span class="record-state live"><i aria-hidden="true"></i>${percent===null?tr(progress?.kind==='postprocess'?'Обработка':'Распознаём'):`${percent}%`}</span>`;}
  return entry.outputPath?`<span class="record-state done">${tr('Текст')}</span>`:`<span class="record-state">${tr('Аудио')}</span>`;
}
function recordDetailHtml(entry:Entry):string {return `<span class="record-detail"><span>${dateLabel(entry.createdAt)}</span>${entry.durationSeconds?`<span>${formatTime(entry.durationSeconds)}</span>`:''}</span>`;}
function recordActionsHtml(entry:Entry):string {
  return `<div class="record-actions"><button class="icon-button" data-rename="${esc(entry.id)}" aria-label="${tr('Переименовать')} ${esc(entry.name)}" title="${tr('Переименовать запись')}" ${disabled()}>${icon('edit')}</button><button class="icon-button" data-delete="${esc(entry.id)}" aria-label="${tr('Удалить')} ${esc(entry.name)}" title="${tr('Удалить запись')}" ${disabled()}>${icon('trash')}</button></div>`;
}
function historyItems(entries:Entry[]):string {
  let group='';
  return entries.map(entry=>{
    const label=groupLabel(entry.createdAt),heading=label!==group?`<div class="history-group">${label}</div>`:'';group=label;
    const locked=busy&&jobRecord()!==entry.id&&active?.id!==entry.id;
    return `${heading}<div class="record-row ${active?.id===entry.id&&(view==='work'||!platform.mobile)?'selected':''}"><button class="record-link" data-history="${esc(entry.id)}" ${locked||platform.os==='unknown'?'disabled':''} title="${esc(entry.name)}"><span class="record-title">${esc(displayName(entry))}</span>${recordDetailHtml(entry)}</button>${recordStateHtml(entry)}${platform.mobile?'':recordActionsHtml(entry)}</div>`;
  }).join('');
}
function historyResults():string {
  const entries=history.filter(entry=>entry.name.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  if (loading) return `<div class="list-empty"><span class="spinner"></span>${tr('Загружаем записи…')}</div>`;
  if (historyError) return `<div class="list-empty">${esc(historyError)}<button class="text-button" id="retry-history">${tr('Повторить')}</button></div>`;
  if (!entries.length) return search?`<div class="list-empty">${tr('Записи не найдены')}</div>`:`<div class="list-empty first-run"><strong>${tr('Пока пусто')}</strong><span>${tr(platform.audioRecording?'Запишите разговор или добавьте файл — здесь появятся аудио и текст.':'Добавьте аудиофайл — здесь появятся аудио и текст.')}</span></div>`;
  return historyItems(entries);
}
function searchFieldHtml():string {return `<label class="search-field">${icon('search')}<input id="history-search" type="search" placeholder="${tr('Поиск')}" value="${esc(search)}" aria-label="${tr('Найти запись')}" autocomplete="off"></label>`;}
function brandHtml():string {return `<span class="brand-mark" aria-hidden="true">${icon('wave')}</span><span>Hearfolio</span>`;}
function desktopModelsNav():string {return platform.localRecognition?`<button class="nav-item ${view==='models'?'active':''}" data-view="models" title="${tr('Модели на устройстве')}">${icon('models')}<span>${tr('Модели')}</span></button>`:'';}
function looseBadgeHtml():string {return loose.length?`<span class="nav-badge" aria-label="${tr('Не обработаны')}: ${loose.length}">${loose.length}</span>`:'';}
function archiveNavHtml():string {return `<button class="nav-item ${view==='archive'?'active':''}" data-view="archive">${icon('archive')}<span>${tr('Архив')}</span>${looseBadgeHtml()}</button>`;}
function sidebarHtml():string {
  if(platform.mobile)return '';
  const locked=recordingPhase!=='idle'||busy||platform.os==='unknown';
  return `<aside class="sidebar" aria-label="${tr('Записи')}"><div class="sidebar-chrome" data-tauri-drag-region><button class="icon-button" id="collapse" title="${tr('Скрыть список')} · ⌘B" aria-label="${tr('Скрыть список')}" aria-expanded="true">${icon('panel')}</button></div><div class="sidebar-body"><button class="brand" id="new" title="${tr('Новая запись')} · ⌘N" ${locked?'disabled':''}>${brandHtml()}</button><div class="sidebar-actions">${platform.audioRecording?`<button class="record-cta" id="quick-record" ${locked?'disabled':''}><span class="record-dot" aria-hidden="true"></span>${tr('Записать')}</button>`:''}<button class="secondary import-cta" data-choose title="${tr('Открыть аудиофайл')} · ⌘O" ${locked?'disabled':''}>${icon('upload')}<span>${tr('Файл')}</span></button></div>${history.length>3||search?searchFieldHtml():''}<div class="record-list" id="history-results">${historyResults()}</div><nav class="sidebar-nav" aria-label="${tr('Приложение')}">${archiveNavHtml()}${desktopModelsNav()}<button class="nav-item ${view==='devices'?'active':''}" data-view="devices">${icon('devices')}<span>${tr('Устройства')}</span></button><button class="nav-item ${view==='settings'||view==='rules'?'active':''}" data-view="settings">${icon('settings')}<span>${tr('Настройки')}</span></button><button class="icon-button" data-theme title="${theme==='dark'?tr("Светлая тема"):tr("Тёмная тема")}" aria-label="${theme==='dark'?tr("Светлая тема"):tr("Тёмная тема")}">${icon(theme==='dark'?'sun':'moon')}</button></nav></div></aside>`;
}
function recordOrbHtml(id:string,label:string):string {return `<button class="record-orb" id="${id}" ${disabled()} aria-label="${label}"><span class="record-orb-core" aria-hidden="true"></span></button>`;}
function captureHtml():string {
  if(recordingPhase!=='idle'){
    const live=['recording','paused'].includes(recordingPhase),paused=recordingPhase==='paused',locked=!live||recordingControlPending?'disabled':'';
    const label=tr(recordingPhase==='starting'?'Подключаем микрофон…':recordingPhase==='stopping'?'Сохраняем аудио…':recordingPhase==='cancelling'?'Отменяем запись…':paused?'Пауза':'Идёт запись');
    return `<section class="recorder ${paused?'recorder-paused':''} ${live?'':'recorder-busy'}" aria-label="${tr('Диктофон')}"><div class="recorder-state"><span class="recorder-dot" aria-hidden="true"></span><span>${label}</span></div><div class="recording-duration" id="recording-duration" aria-label="${tr('Длительность записи')}">${recordingDuration()}</div>${recordingMeterHtml()}<div class="recorder-actions"><button type="button" class="round-button" id="cancel-recording" ${locked} aria-label="${tr('Отменить запись')}" title="${tr('Отменить запись')}">${icon('close')}</button><button type="button" class="record-orb recording" id="stop-recording" ${locked} aria-label="${tr('Остановить и сохранить')}"><span class="stop-symbol" aria-hidden="true"></span></button><button type="button" class="round-button" id="pause-recording" ${locked} aria-label="${tr(paused?'Продолжить':'Пауза')}" title="${tr(paused?'Продолжить':'Пауза')}">${icon(paused?'play':'pause')}</button></div><p class="recorder-hint">${tr(platform.os==='android'?'Запись продолжится, даже если заблокировать экран.':'Нажмите ■, чтобы сохранить запись.')}</p></section>`;
  }
  if(pendingRecording)return `<section class="welcome"><h1>${tr('Аудио записано')}</h1><p>${tr('Сохраните запись, чтобы расшифровать её.')}</p><div class="welcome-actions"><button class="primary large" id="retry-recording" ${disabled()}>${tr('Сохранить запись')}</button><button class="text-button danger" id="discard-recording" ${disabled()}>${tr('Удалить')}</button></div></section>`;
  return `<section class="welcome" id="drop-area">${platform.audioRecording?`${recordOrbHtml('start-recording',tr('Начать запись'))}<h1>${tr('Нажмите, чтобы записать')}</h1><p>${tr('Или')} <button class="link-button" data-choose ${disabled()}>${tr('выберите аудиофайл')}</button>${platform.mobile?'':tr(' — его можно просто перетащить сюда')}.</p>`:`<button class="drop-target" data-choose ${disabled()}>${icon('upload')}</button><h1>${tr('Добавьте аудиофайл')}</h1><p>${tr('Перетащите файл сюда или')} <button class="link-button" data-choose ${disabled()}>${tr('выберите на диске')}</button>.</p>`}<small>${audioFormats}</small></section>`;
}
function stageHtml():string {
  const transcribing=busy&&progress?.kind==='transcribe',percent=transcribing?progressPercent():null;
  return `<section class="stage ${transcribing?'is-running':''}" aria-live="polite"><div class="stage-wave ${transcribing&&percent===null?'indeterminate':''}" style="--progress:${percent??0}%" aria-hidden="true">${waveBars(active?.id||'hearfolio',42).map(level=>`<i style="--level:${level.toFixed(3)}"></i>`).join('')}</div><h2>${tr(transcribing?'Расшифровываем…':'Готово к расшифровке')}</h2><p>${tr(transcribing?(platform.mobile?'Можно свернуть приложение — работа продолжится, а текст появится здесь.':'Текст будет появляться здесь по мере готовности.'):'Выберите модель внизу и нажмите «Расшифровать».')}</p></section>`;
}
function outputHtml():string {
  if(recordingPhase!=='idle'||!active)return captureHtml();
  if(outputTab==='results')return resultsHtml();
  const visible=shownText();
  if (visible) return `<article class="transcript" id="transcript-text" lang="${esc(currentLanguage())}">${esc(visible)}</article>`;
  return stageHtml();
}
function resultMeta():string {return busy&&progress?.kind==='transcribe'?tr("Расшифровываем…"):text?`${countLabel(words(),'word')}${resultSaved?'':tr(" · не сохранено")}`:tr("Без текста");}
function recordMetaHtml():string {
  if(!active)return '';
  return [dateLabel(active.createdAt),active.durationSeconds?formatTime(active.durationSeconds):'',active.sizeBytes?sizeLabel(active.sizeBytes):'',resultMeta()].filter(Boolean).map(part=>`<span>${esc(part)}</span>`).join('');
}
function providerControlsHtml():string {
  if(!platform.localRecognition)return cloudControlsHtml();
  return `<div class="segmented" role="group" aria-label="${tr('Способ распознавания')}"><button type="button" data-provider="local" aria-pressed="${configuration.provider==='local'}" ${disabled()}>${tr('На устройстве')}</button><button type="button" data-provider="openrouter" aria-pressed="${configuration.provider==='openrouter'}" ${disabled()}>OpenRouter</button></div>${configuration.provider==='local'?`<label class="model-chip"><span class="sr-only">${tr('Локальная модель')}</span><select id="local-model" ${disabled()}>${models.map(model=>`<option value="${model.id}" ${configuration.model===model.id?'selected':''}>${model.name}${model.installed?'':tr(" · скачать")}</option>`).join('')}</select>${icon('chevron')}</label>`:cloudControlsHtml()}`;
}
function cloudControlsHtml():string {
  const model=cloudModels.find(item=>item.id===configuration.model),label=model?.name||configuration.model||tr('Выбрать модель');
  return `<div class="cloud-picker"><button type="button" class="model-chip" id="cloud-picker-toggle" aria-haspopup="dialog" aria-expanded="${catalogOpen}" title="${esc(configuration.model||'')}" ${disabled()}>${icon('cloud')}<span>${esc(label)}</span>${icon('chevron')}</button>${catalogOpen&&!platform.mobile?catalogHtml():''}</div>`;
}
function catalogHtml():string {
  const matches=cloudModels.filter(model=>`${model.name} ${model.id}`.toLocaleLowerCase().includes(catalogSearch.toLocaleLowerCase()));
  return `<section class="model-catalog" role="dialog" aria-label="${tr('Модели OpenRouter')}"><div class="catalog-heading"><strong>${tr('Модель для расшифровки')}</strong><button type="button" class="icon-button" id="catalog-close" aria-label="${tr('Закрыть')}">${icon('close')}</button></div><label class="search-field">${icon('search')}<input type="search" id="catalog-search" placeholder="${tr('Найти модель')}" value="${esc(catalogSearch)}" aria-label="${tr('Найти модель')}"></label><div id="catalog-results">${catalogResults(matches)}</div><button type="button" class="text-button" id="catalog-refresh" ${cloudLoading?'disabled':''}>${tr('Обновить каталог')}</button></section>`;
}
function catalogResults(matches:CloudModel[]):string {
  if(cloudLoading&&!cloudModels.length)return `<div class="catalog-empty"><span class="spinner"></span> ${tr('Загружаем модели…')}</div>`;
  const warning=catalogError?`<div class="catalog-empty" role="alert">${esc(catalogError)}</div>`:'';
  if(catalogError&&!cloudModels.length)return warning;
  if(!matches.length)return `<div class="catalog-empty">${cloudModels.length?tr("Модели не найдены"):tr("Нет моделей с поддержкой аудио")}</div>`;
  return `${warning}<div class="catalog-list">${matches.map(model=>`<button type="button" class="catalog-option ${configuration.model===model.id?'selected':''}" data-cloud-model="${esc(model.id)}"><span><strong>${esc(model.name)}</strong><small>${esc(model.id)} · ${tr(model.preferredMode==='transcription'?'файл целиком':'потоковый текст')}</small></span>${configuration.model===model.id?icon('check'):''}</button>`).join('')}</div>`;
}
function runButtonHtml(issue:string):string {
  const again=Boolean(text&&resultSaved)&&!canResume(),label=canResume()?`${tr('Продолжить')} (${resume!.done}/${resume!.total})`:tr(again?'Расшифровать заново':'Расшифровать');
  return `<button class="${again?'secondary':'primary'} run-button" id="run" ${busy||!active||issue?'disabled':''} title="${esc(issue||label+' · ⌘ Enter')}">${again?icon('history'):''}<span>${label}</span></button>`;
}
function requirementHtml(issue:string):string {
  if(!issue)return '';
  const needsDownload=configuration.provider==='local'&&runtime.ffmpeg&&!models.find(model=>model.id===localChoice)?.installed;
  const needsCatalog=configuration.provider==='openrouter'&&(platform.nativeAudio||runtime.ffmpeg)&&settings.hasToken;
  const action=needsDownload?`data-download="${localChoice}"`:needsCatalog?(catalogError?'id="retry-catalog"':'id="open-catalog"'):configuration.provider==='openrouter'&&(platform.nativeAudio||runtime.ffmpeg)?'data-view="settings" data-settings-target="recognition"':platform.localRecognition?'data-view="models"':'data-view="settings" data-settings-target="recognition"';
  return `<div class="requirement" role="note">${icon('alert')}<span>${esc(issue)}</span>${runtimeLoaded?`<button class="text-button" ${action} ${busy||cloudLoading?'disabled':''}>${needsDownload?tr("Скачать модель"):needsCatalog?tr(catalogError?'Обновить каталог':'Выбрать модель'):tr("Настроить")}</button>`:''}</div>`;
}
function composerHtml(issue:string):string {
  if(recordingPhase!=='idle'||!active)return busy&&progress?`<div class="dock"><div id="progress">${progressHtml()}</div></div>`:'';
  if(busy&&progress)return `<div class="dock"><div id="progress">${progressHtml()}</div></div>`;
  if(outputTab==='results')return '';
  return `<div class="dock"><div id="progress"></div>${requirementHtml(issue)}<div class="dock-row"><div class="dock-model">${providerControlsHtml()}</div>${runButtonHtml(issue)}</div></div>`;
}
function recordHeaderHtml():string {
  if(!active)return '';
  return `<header class="record-header"><div class="record-heading"><div class="record-name-row"><h1 title="${esc(active.name)}">${esc(displayName(active))}</h1><button class="icon-button" data-rename="${esc(active.id)}" aria-label="${tr('Переименовать запись')}" title="${tr('Переименовать запись')}" ${disabled()}>${icon('edit')}</button></div><div class="record-meta" id="text-meta">${recordMetaHtml()}</div></div><div class="record-tools"><button type="button" class="secondary" id="record-share" aria-haspopup="dialog" aria-expanded="false" aria-controls="record-actions-dialog">${icon('share')}<span>${tr('Поделиться')}</span></button>${platform.mobile?`<button type="button" class="icon-button" data-delete="${esc(active.id)}" aria-label="${tr('Удалить запись')}" ${disabled()}>${icon('trash')}</button>`:''}</div></header>`;
}
function outputTabsHtml():string {
  if(!active||!(text||active.results?.length))return '';
  return `<div class="output-tabs"><div class="output-tab-buttons" role="tablist" aria-label="${tr('Текст записи')}"><button role="tab" id="transcript-tab" data-output-tab="transcript" aria-selected="${outputTab==='transcript'}">${tr('Текст')}</button><button role="tab" id="results-tab" data-output-tab="results" aria-selected="${outputTab==='results'}">${tr('Выжимки')}${active.results?.length?` <small>${active.results.length}</small>`:''}</button></div>${outputTab==='transcript'&&text?`<div class="output-tools"><button type="button" class="ghost-button" id="copy-transcript" ${busy&&progress?.kind==='transcribe'?'disabled':''}>${icon('copy')}<span>${tr('Копировать')}</span></button>${resultSaved?`<button type="button" class="ghost-button" id="process-shortcut" ${busy?'disabled':''}>${icon('file')}<span>${tr('Сделать выжимку')}</span></button>`:''}</div>`:''}</div>`;
}
function workHtml():string {
  const issue=readiness(),source=active&&!platform.mobile?audioSource(active):'';
  const recording=recordingPhase!=='idle'||!active;
  return `<section class="workspace ${recording?'is-capture':''}">
    ${recordingPhase==='idle'?transferBarHtml(active):''}
    ${!recording?`${recordHeaderHtml()}<div class="audio-bar ${platform.mobile?'native-audio-bar':''}">${platform.mobile?nativePlayerHtml():source?`<audio id="audio" controls controlslist="nodownload noplaybackrate" preload="metadata" src="${esc(source)}" aria-label="${tr('Прослушать ')}${esc(active!.name)}"></audio>`:''}<p id="playback-error" class="playback-error" role="alert"></p></div>${outputTabsHtml()}`:''}
    <div class="transcript-scroll" id="result" ${active&&!recording&&(text||active.results?.length)?`role="tabpanel" aria-labelledby="${outputTab==='transcript'?'transcript-tab':'results-tab'}"`:''}>${outputHtml()}${!recording&&text&&resultSaved&&outputTab==='transcript'?postprocessHtml():''}</div>
    ${previousResult?.saved&&!busy&&!resultSaved?`<div class="restore-result"><span>${tr('Предыдущий текст сохранён в архиве.')}</span><button class="text-button" id="restore">${tr('Вернуть его')}</button></div>`:''}
    ${composerHtml(issue)}
  </section>`;
}
function nativePlayerHtml():string {
  const duration=playerState.duration||active?.durationSeconds||0;
  return `<div class="native-player" aria-label="${tr('Прослушать ')}${esc(active?.name||'')}"><button type="button" id="native-play" class="play-button" aria-label="${tr(playerState.playing?'Пауза':'Воспроизвести')}" ${playerLoading||playerError?'disabled':''}>${playerLoading?'<span class="spinner"></span>':icon(playerState.playing?'pause':'play')}</button><div class="player-track"><input type="range" id="native-seek" min="0" max="${Math.max(duration,1)}" value="${playerState.currentTime}" step="0.1" aria-label="${tr('Позиция воспроизведения')}" ${playerLoading||playerError||!duration?'disabled':''}><div><span id="native-position">${formatTime(playerState.currentTime)}</span><span id="native-duration">${duration?formatTime(duration):'—'}</span></div></div></div>${playerError?`<div class="player-error" role="alert"><span>${esc(playerError)}</span><button type="button" class="text-button" id="retry-playback">${tr('Повторить')}</button></div>`:''}`;
}
function homeHtml():string {
  const job=jobRecord()?history.find(entry=>entry.id===jobRecord()):null;
  return `<section class="home">${job?`<button class="job-banner" data-history="${esc(job.id)}"><span class="spinner"></span><span><strong>${esc(displayName(job))}</strong><small>${esc(progress?.stage||'')}${progressPercent()!==null?` · ${progressPercent()}%`:''}</small></span>${icon('chevron')}</button>`:''}${history.length>5||search?searchFieldHtml():''}<div class="record-list" id="history-results">${historyResults()}</div></section><div class="home-dock"><button class="dock-side" data-choose ${disabled()}>${icon('upload')}<span>${tr('Файл')}</span></button>${platform.audioRecording?recordOrbHtml('quick-record',tr('Начать запись')):''}<span class="dock-side" aria-hidden="true"></span></div>`;
}
function modelsHtml():string {
  const installationCommand=platform.os==='linux'?'sudo apt install ffmpeg':'brew install ffmpeg whisper.cpp';
  const installationHelp=platform.os==='linux'?tr('На Debian и Ubuntu установите FFmpeg этой командой. Whisper установите по инструкции проекта whisper.cpp.'):tr('Команда использует Homebrew. Модель скачивается отдельно ниже.');
  return `<div class="page-heading"><div><h1>${tr('Модели на устройстве')}</h1><p>${tr('Скачайте один раз. Дальше записи можно распознавать без интернета.')}</p></div></div><div class="runtime-panel"><div><h2>${tr('Готовность к распознаванию')}</h2><button class="icon-button" id="retry-runtime" title="${tr('Проверить снова')}" aria-label="${tr('Проверить готовность снова')}">${icon('history')}</button></div><div class="runtime-checks">${[{name:'FFmpeg',ready:runtime.ffmpeg},{name:'Whisper',ready:runtime.whisper},{name:'NeMo',ready:runtime.nemo}].map(item=>`<span class="runtime-check ${item.ready?'ready':''}">${icon(item.ready?'check':'alert')}${item.name}<small>${runtimeLoaded?(item.ready?tr("Готов"):tr("Не установлен")):tr("Проверяем")}</small></span>`).join('')}</div>${runtimeLoaded&&(!runtime.ffmpeg||!runtime.whisper)?`<p>${tr('Для Whisper установите инструменты в Терминале, затем проверьте готовность снова.')}</p><div class="install-command"><code>${installationCommand}</code><button class="icon-button" id="copy-install" title="${tr('Копировать команду')}" aria-label="${tr('Копировать команду установки')}">${icon('copy')}</button></div><p class="small-copy">${installationHelp}</p>${platform.os==='linux'?`<a href="https://github.com/ggml-org/whisper.cpp" target="_blank" rel="noreferrer">${tr('Инструкция whisper.cpp')}${icon('chevron')}</a>`:''}`:''}</div><div class="model-list">${models.map(model=>`<article class="model-card"><div class="model-copy"><h2>${model.name}${localChoice===model.id?`<span class="badge">${tr('Выбрана')}</span>`:''}</h2><p>${tr(model.detail)}</p><small>${tr(model.size)}${model.installed?tr(" · Скачана"):''}</small></div>${model.installed?`<button class="secondary" data-select="${model.id}" ${busy||localChoice===model.id?'disabled':''}>${localChoice===model.id?icon('check')+tr("Выбрана"):tr("Использовать")}</button>`:`<button class="secondary" data-download="${model.id}" ${disabled()}>${icon('download')}${tr("Скачать")}</button>`}</article>`).join('')}</div><div class="help-note"><h2>${tr('Для Nemotron нужен NeMo')}</h2><p>${tr('Установите NeMo-Speech.cpp по инструкции проекта. Движок добавит команду <code>nemo-speech</code>; затем проверьте готовность снова.')}</p><a href="https://github.com/NVIDIA/NeMo-Speech.cpp" target="_blank" rel="noreferrer">${tr('Инструкция NeMo-Speech.cpp ')}${icon('chevron')}</a></div>`;
}
const looseLocked=()=>busy||recordingPhase!=='idle'||platform.os==='unknown'?'disabled':'';
function looseName(item:LooseRecording):string {return recordingName({path:item.path},item.createdAt);}
function archiveHtml():string {
  const locked=looseLocked(),detail=(parts:(string|false|undefined)[])=>`<span class="archive-meta">${parts.filter(Boolean).map(part=>`<span>${esc(String(part))}</span>`).join('')}</span>`;
  const looseItems=loose.map(item=>`<article class="archive-item"><div class="archive-item-head"><div class="archive-item-title"><strong>${esc(recordNameParts(looseName(item),item.path).title)}</strong>${detail([dateLabel(item.createdAt),item.durationSeconds?formatTime(item.durationSeconds):'',sizeLabel(item.sizeBytes)])}</div><span class="archive-status failed">${tr('Не обработана')}</span></div><div class="archive-item-actions"><button type="button" class="primary" data-loose-retry="${esc(item.path)}" ${locked}>${icon('history')}<span>${tr('Повторить')}</span></button>${platform.mobile?`<button type="button" class="secondary" data-loose-share="${esc(item.path)}" ${locked}>${icon('share')}<span>${tr('Поделиться')}</span></button>`:''}<button type="button" class="secondary" data-loose-save="${esc(item.path)}" ${locked}>${icon('download')}<span>${tr('Сохранить')}</span></button><button type="button" class="icon-button danger" data-loose-delete="${esc(item.path)}" aria-label="${tr('Удалить запись')}" title="${tr('Удалить запись')}" ${locked}>${icon('trash')}</button></div></article>`).join('');
  const failed=looseLoading&&!loose.length?`<div class="list-empty"><span class="spinner"></span>${tr('Ищем необработанные записи…')}</div>`:looseError?`<div class="list-empty" role="alert">${esc(looseError)}<button class="text-button" id="retry-loose">${tr('Повторить')}</button></div>`:looseItems?`<p class="archive-hint">${tr('Обработка этих записей не завершилась. Аудио сохранено: повторите обработку или выгрузите файл.')}</p>${looseItems}`:'';
  const saved=history.map(entry=>`<article class="archive-item"><div class="archive-item-head"><div class="archive-item-title"><strong title="${esc(entry.name)}">${esc(displayName(entry))}</strong>${detail([dateLabel(entry.createdAt),entry.durationSeconds?formatTime(entry.durationSeconds):'',entry.sizeBytes?sizeLabel(entry.sizeBytes):''])}</div>${jobRecord()===entry.id?recordStateHtml(entry):`<span class="archive-status ${entry.outputPath?'done':''}">${tr(entry.outputPath?'Расшифрована':'Без текста')}</span>`}</div><div class="archive-item-actions"><button type="button" class="secondary" data-history="${esc(entry.id)}" ${busy&&jobRecord()!==entry.id?'disabled':''}>${icon('chevron')}<span>${tr(entry.outputPath?'Открыть':'Расшифровать')}</span></button>${platform.mobile?`<button type="button" class="secondary" data-archive-share="${esc(entry.id)}" ${locked}>${icon('share')}<span>${tr('Поделиться')}</span></button>`:''}<button type="button" class="secondary" data-archive-save="${esc(entry.id)}" ${locked}>${icon('download')}<span>${tr('Сохранить')}</span></button></div></article>`).join('');
  return `<div class="page-heading"><div><h1>${tr('Архив')}</h1><p>${tr('Все записи на этом устройстве и статус их обработки.')}</p></div></div>${failed?`<section class="archive-section"><h2>${tr('Не обработаны')}${loose.length?` <small>${loose.length}</small>`:''}</h2>${failed}</section>`:''}<section class="archive-section"><h2>${tr('В архиве')}${history.length?` <small>${history.length}</small>`:''}</h2>${historyError?`<div class="list-empty">${esc(historyError)}<button class="text-button" id="retry-history">${tr('Повторить')}</button></div>`:saved||`<div class="list-empty">${tr('Пока пусто')}</div>`}</section>`;
}
async function refreshLoose(){
  if(platform.os==='unknown')return;
  looseLoading=true;if(view==='archive')render();
  try{loose=(await listLooseRecordings()).sort((left,right)=>right.createdAt-left.createdAt);looseError='';}
  catch(error){looseError=errorText(error);}
  finally{looseLoading=false;render();}
}
async function retryLoose(path:string){
  const item=loose.find(candidate=>candidate.path===path);if(!item||looseLocked())return;
  if(active&&!await newRecord())return;
  try{await adoptLooseRecording(path);}catch(error){notice(errorText(error),true);await refreshLoose();return;}
  const imported=await importSelection({path,name:looseName(item)});
  if(!imported)view='archive';
  await refreshLoose();
}
async function deleteLoose(path:string){
  if(looseLocked()||!loose.some(item=>item.path===path))return;
  if(!await confirmAction(tr('Удалить запись?'),tr('Это аудио ещё не попало в архив. После удаления его нельзя будет восстановить.'),tr('Удалить запись')))return;
  try{await adoptLooseRecording(path);await discardAudioRecording({path});}catch(error){notice(errorText(error),true);}
  await refreshLoose();
}
function bindArchive(){
  document.querySelector('#retry-loose')?.addEventListener('click',()=>{void refreshLoose();});
  document.querySelectorAll<HTMLElement>('[data-loose-retry]').forEach(element=>element.onclick=()=>{void retryLoose(element.dataset.looseRetry!);});
  document.querySelectorAll<HTMLElement>('[data-loose-delete]').forEach(element=>element.onclick=()=>{void deleteLoose(element.dataset.looseDelete!);});
  const exportLoose=(path:string,share:boolean)=>{const item=loose.find(candidate=>candidate.path===path);if(item)void exportLooseRecording(path,looseName(item),share).catch(error=>notice(errorText(error),true));};
  document.querySelectorAll<HTMLElement>('[data-loose-share]').forEach(element=>element.onclick=()=>exportLoose(element.dataset.looseShare!,true));
  document.querySelectorAll<HTMLElement>('[data-loose-save]').forEach(element=>element.onclick=()=>exportLoose(element.dataset.looseSave!,false));
  const saved=(id:string)=>history.find(entry=>entry.id===id);
  document.querySelectorAll<HTMLElement>('[data-archive-share]').forEach(element=>element.onclick=()=>{const entry=saved(element.dataset.archiveShare!);if(entry)void shareAudio(entry).catch(error=>notice(errorText(error),true));});
  document.querySelectorAll<HTMLElement>('[data-archive-save]').forEach(element=>element.onclick=()=>{const entry=saved(element.dataset.archiveSave!);if(entry)void saveAudio(entry).catch(error=>notice(errorText(error),true));});
}
function settingsHtml():string {
  return renderSettings({settings,draft:settingsDraft,section:settingsSection||(!platform.mobile&&window.innerWidth>760?'general':null),mobile:platform.mobile,android:platform.os==='android',theme,busy,saving:settingsSaving,error:settingsError,customStorage:platform.customStorage,transferMvp:Boolean(platform.transferMvp),version:applicationVersion,devicesHtml:transferSettingsHtml(),updatesHtml:updatesHtml(),recordCount:history.length});
}
function headerTitle():string {
  if(view==='work')return tr(recordingPhase!=='idle'?'Диктофон':active?'Запись':'Новая запись');
  return tr(title[view]);
}
function mobileHeaderHtml():string {
  const home=view==='history',back=view==='devices'||view==='rules'?'settings':'history';
  return `<header class="main-header mobile-header">${home?`<span class="brand">${brandHtml()}</span>`:`<button type="button" class="icon-button back-button" ${view==='settings'&&settingsSection?'data-settings-back':`data-back="${back}"`} aria-label="${tr('Назад')}" ${recordingPhase!=='idle'?'hidden':''}>${icon('back')}</button><span class="header-title">${esc(headerTitle())}</span>`}<div class="header-status"><div id="update-header">${updateHeaderHtml()}</div>${home?`<button type="button" class="icon-button badge-host" data-view="archive" aria-label="${tr('Архив')}">${icon('archive')}${looseBadgeHtml()}</button><button type="button" class="icon-button" data-view="settings" aria-label="${tr('Настройки')}">${icon('settings')}</button>`:''}</div></header>`;
}
function updateHeaderHtml():string {
  return pendingUpdate||androidRelease||updateInstalled?`<button type="button" class="text-button update-shortcut" data-view="settings" data-settings-target="app">${icon('download')}<span>${tr(updateInstalled?'Нужен перезапуск':'Доступно обновление')}</span></button>`:'';
}
function updatesHtml():string {
  if(platform.transferMvp)return `<p>${currentLanguage()==='ru'?'Тестовая сборка: обновления отключены.':'Test build: updates are disabled.'}</p>`;
  if(platform.os!=='android'&&platform.os!=='macos'&&platform.os!=='linux'&&platform.os!=='windows')return `<p>${tr('Обновления на этой платформе недоступны.')}</p>`;
  if(updateInstalled)return `<p>${tr('Обновление установлено. Перезапустите приложение, когда закончите работу.')}</p><button type="button" class="secondary" id="restart-update" ${busy?'disabled':''}>${tr('Перезапустить приложение')}</button>`;
  const version=pendingUpdate?.version||androidRelease?.version;
  return `<p>${platform.mobile?tr('На Android обновление устанавливается вручную из APK. Ваш архив останется в приложении.'):tr('Обновления проверяются автоматически. Установка и перезапуск выполняются по вашему запросу.')}</p>${version?`<div class="update-offer"><span>${tr('Доступно обновление')} <strong>${esc(version)}</strong></span><button type="button" class="secondary" id="install-update" ${busy||updateChecking?'disabled':''}>${tr(platform.mobile?'Открыть APK':'Установить обновление')}</button></div>`:''}${updateError?`<p class="update-error" role="alert">${esc(tr(updateError))}</p>`:updateChecked&&!version?`<p class="update-checked">${tr('Установлена последняя версия')}</p>`:''}<button type="button" class="text-button" id="check-update" ${busy||updateChecking?'disabled':''}>${tr(updateChecking?'Проверяем обновления…':'Проверить обновления')}</button>`;
}
function refreshUpdateUi() {
  const section=document.querySelector('#about-updates');if(section){section.innerHTML=updatesHtml();bindUpdates();}
  const header=document.querySelector('#update-header');if(header){header.innerHTML=updateHeaderHtml();header.querySelector<HTMLElement>('[data-view]')?.addEventListener('click',()=>changeView('settings','app'));}
}
async function checkUpdates(manual=false) {
  if(platform.transferMvp||busy||updateChecking||updateInstalled||(!platform.mobile&&platform.os!=='macos'&&platform.os!=='linux'&&platform.os!=='windows')||platform.os==='ios')return;
  if(preview&&!manual)return;
  updateChecking=true;updateError='';lastUpdateCheck=Date.now();refreshUpdateUi();
  try{
    if(platform.os==='android'){
      const release=await latestAndroidRelease();if(release){const url=new URL(release.url);if(url.protocol!=='https:'||url.hostname!=='github.com'||!url.pathname.startsWith('/bubaley/hearfolio/releases/'))throw new Error('errors.updateUrlInvalid');}androidRelease=release&&isNewerVersion(release.version,applicationVersion)?release:null;
    }else if(!platform.mobile){
      const update=await checkDesktopUpdate();if(pendingUpdate)await pendingUpdate.close().catch(()=>{});pendingUpdate=update&&isNewerVersion(update.version,applicationVersion)?update:null;if(update&&!pendingUpdate)await update.close().catch(()=>{});
    }
    updateChecked=true;
    if(manual&&!pendingUpdate&&!androidRelease)notice(tr('Установлена последняя версия'));
    const availableVersion=pendingUpdate?.version||androidRelease?.version;if(availableVersion&&(manual||availableVersion!==lastNotifiedUpdate)){lastNotifiedUpdate=availableVersion;notice(`${tr('Доступно обновление')} ${availableVersion}`);}
  }catch(error){updateError='Не удалось проверить обновления. Повторите попытку позже.';if(manual)notice(`${tr(updateError)} ${errorText(error)}`,true);}
  finally{updateChecking=false;refreshUpdateUi();}
}
async function installUpdate() {
  if(busy||updateChecking)return;
  if(platform.mobile){if(androidRelease)await openExternal(androidRelease.url).catch(error=>notice(errorText(error),true));return;}
  if(!pendingUpdate)return;
  const selected=pendingUpdate;
  beginOperation('update',tr('Скачиваем обновление'));render();
  try{
    await settingsSaveQueue;await configurationSaveQueue;
    await selected.install(info=>{if(progress){progress.current=info.current;progress.total=info.total;progress.stage=tr(info.finished?'Устанавливаем обновление':'Скачиваем обновление');updateProgress();}});
    updateInstalled=true;installedUpdateVersion=selected.version;await selected.close().catch(()=>{});pendingUpdate=null;notice(tr('Обновление установлено. Перезапустите приложение, когда закончите работу.'));
  }catch(error){notice(`${tr('Не удалось установить обновление: ')}${errorText(error)}`,true);}
  finally{endOperation();render();}
}
async function restartAfterUpdate(requireUpdate=true) {
  if(busy||(requireUpdate&&!updateInstalled)||!['macos','linux','windows'].includes(platform.os))return;
  if(!await mayLeaveSettings()||!await mayReplaceRecord(false))return;
  if(!await confirmAction(tr('Перезапустить приложение?'),tr('Текущая работа будет завершена. Сохранённые записи останутся в архиве.'),tr('Перезапустить'),false))return;
  if(busy)return;
  busy=true;render();
  try{await settingsSaveQueue;await configurationSaveQueue;if(pendingRecording){await discardAudioRecording(pendingRecording);pendingRecording=null;}await restartApp();if(preview){if(updateInstalled)applicationVersion=installedUpdateVersion;updateInstalled=false;busy=false;render();}}catch(error){busy=false;render();notice(`${tr('Не удалось перезапустить приложение: ')}${errorText(error)}`,true);}
}
function bindUpdates() {
  document.querySelector('#check-update')?.addEventListener('click',()=>checkUpdates(true));
  document.querySelector('#install-update')?.addEventListener('click',installUpdate);
  document.querySelector('#restart-update')?.addEventListener('click',()=>restartAfterUpdate());
}
function preservePlayback() {
  const audio=document.querySelector<HTMLAudioElement>('#audio');
  if (audio) {audioPosition=audio.currentTime;audioWasPlaying=!audio.paused;}
}
function activityLabel():string {
  if(!busy||recordingPhase!=='idle'||!progress)return transferActivityLabel();
  return progress.kind==='download'?tr("Скачиваем модель"):progress.kind==='import'?tr("Добавляем запись"):progress.kind==='storage'?tr('Переносим архив'):progress.kind==='update'?tr('Устанавливаем обновление'):progress.kind==='postprocess'?tr('Обрабатываем текст…'):tr('Расшифровываем');
}
function desktopHeaderHtml():string {
  const activity=activityLabel();
  return `<header class="main-header" data-tauri-drag-region><div class="header-title" data-tauri-drag-region><button class="icon-button" id="expand" title="${tr('Показать список')} · ⌘B" aria-label="${tr('Показать список')}" aria-expanded="false">${icon('panel')}</button><span data-tauri-drag-region>${esc(headerTitle())}</span></div><div class="header-status"><div id="update-header">${updateHeaderHtml()}</div>${activity&&view!=='work'?`<span class="header-activity"><span class="spinner"></span>${esc(activity)}</span>`:''}${preview?`<span class="preview-label">${tr('Предпросмотр')}</span>`:''}</div></header>`;
}
function surfaceHtml():string {
  if(view==='history'&&platform.mobile)return homeHtml();
  if(view==='work'||view==='history')return workHtml();
  return `<div class="page-scroll ${view==='devices'&&!platform.mobile?'devices-scroll':''}"><div class="page-content ${view==='devices'?'devices-page':''}">${view==='archive'?archiveHtml():view==='models'?modelsHtml():view==='devices'?devicesHtml():view==='rules'?rulesHtml():settingsHtml()}${busy&&progress?`<div id="progress">${progressHtml()}</div>`:''}</div></div>`;
}
let booted=false;
function render() {
  // Until the platform is known, show only the mark so Android never flashes the desktop layout.
  if(!booted){app.innerHTML=`<div class="boot"><span class="brand-mark">${icon('wave')}</span></div>`;return;}
  preservePlayback();
  if(platform.mobile&&view==='work'&&!active&&recordingPhase==='idle'&&!pendingRecording&&!busy)view='history';
  const kept=document.querySelector<HTMLAudioElement>('#audio');
  document.documentElement.dataset.platform=platform.os;
  app.innerHTML=`<div ${(platform.mobile&&catalogOpen)||textPickerOpen()?'inert':''} class="shell ${sidebarCollapsed?'sidebar-collapsed':''} ${platform.mobile?'mobile-platform':''} view-${view}">${sidebarHtml()}<main>${platform.mobile?mobileHeaderHtml():desktopHeaderHtml()}<div class="main-surface ${view==='work'||view==='history'?'work-surface':`page-surface ${view}-surface`}">${surfaceHtml()}<div id="status" class="notice ${isError?'error':''}" role="status" aria-live="polite"></div></div></main></div>${platform.mobile&&catalogOpen?`<div class="catalog-backdrop" id="catalog-backdrop">${catalogHtml()}</div>`:''}${textPickerHtml()}`;
  // Re-use the playing <audio> element so unrelated re-renders never interrupt playback.
  const slot=document.querySelector<HTMLAudioElement>('#audio');let reused=false;
  if(kept&&slot&&kept.getAttribute('src')===slot.getAttribute('src')){slot.replaceWith(kept);reused=true;}
  recorderPalette=null;requestFrames();
  bind();updateNotice();if(!reused)restorePlayback();if(view==='work'&&active&&platform.mobile&&recordingPhase==='idle')void ensureNativePlayer();else stopPlayerPolling();
}
function restorePlayback() {
  const audio=document.querySelector<HTMLAudioElement>('#audio');
  if (!audio) return;
  audio.addEventListener('loadedmetadata',()=>{if(audioPosition&&audioPosition<audio.duration)audio.currentTime=audioPosition;if(audioWasPlaying)audio.play().catch(()=>{});});
  audio.addEventListener('error',()=>{const error=document.querySelector('#playback-error');if(error)error.textContent=tr('Не удалось воспроизвести аудио. Попробуйте открыть запись снова. Файл можно распознать.');});
}
function stopPlayerPolling(){if(playerPoll)clearTimeout(playerPoll);playerPoll=null;}
function updateNativePlayer(){
  const duration=playerState.duration||active?.durationSeconds||0,position=document.querySelector('#native-position'),total=document.querySelector('#native-duration'),seek=document.querySelector<HTMLInputElement>('#native-seek'),button=document.querySelector<HTMLButtonElement>('#native-play');
  if(position)position.textContent=formatTime(playerState.currentTime);if(total)total.textContent=duration?formatTime(duration):'—';
  if(seek){seek.max=String(Math.max(duration,1));if(document.activeElement!==seek)seek.value=String(Math.min(playerState.currentTime,duration));seek.disabled=playerLoading||!!playerError||!duration;}
  if(button){button.innerHTML=playerLoading?'<span class="spinner"></span>':icon(playerState.playing?'pause':'play');button.setAttribute('aria-label',tr(playerState.playing?'Пауза':'Воспроизвести'));button.disabled=playerLoading||!!playerError;}
}
async function pollNativePlayer(){
  const generation=playerGeneration;if(view!=='work'||!active||active.id!==playerId||!playerState.playing)return;
  try{const state=await nativePlaybackState();if(generation!==playerGeneration)return;playerState=state;updateNativePlayer();if(state.playing&&view==='work')playerPoll=setTimeout(()=>{void pollNativePlayer();},250);}
  catch(error){if(generation!==playerGeneration)return;playerError=errorText(error);playerState.playing=false;render();}
}
async function ensureNativePlayer(force=false){
  if(!active||!platform.mobile)return;
  if(!force&&playerId===active.id){if(playerState.playing&&!playerPoll)void pollNativePlayer();return;}
  stopPlayerPolling();const entry=active,generation=++playerGeneration;playerId=entry.id;playerLoading=true;playerError='';playerState={duration:entry.durationSeconds||0,currentTime:0,playing:false};updateNativePlayer();
  try{const state=await prepareNativePlayback(entry);if(generation!==playerGeneration)return;playerState=state;}
  catch(error){if(generation!==playerGeneration)return;playerError=errorText(error);}
  finally{if(generation===playerGeneration){playerLoading=false;if(view==='work')render();}}
}
function bindNativePlayer(){
  document.querySelector('#retry-playback')?.addEventListener('click',()=>{void ensureNativePlayer(true);});
  document.querySelector('#native-play')?.addEventListener('click',async()=>{if(playerLoading||playerError)return;try{if(playerState.playing)await pauseNativePlayback();else await playNativePlayback();playerState=await nativePlaybackState();stopPlayerPolling();updateNativePlayer();if(playerState.playing)void pollNativePlayer();}catch(error){playerError=errorText(error);playerState.playing=false;render();}});
  document.querySelector<HTMLInputElement>('#native-seek')?.addEventListener('input',event=>{const value=Number((event.target as HTMLInputElement).value);const position=document.querySelector('#native-position');if(position)position.textContent=formatTime(value);});
  document.querySelector<HTMLInputElement>('#native-seek')?.addEventListener('change',async event=>{try{await seekNativePlayback(Number((event.target as HTMLInputElement).value));playerState=await nativePlaybackState();updateNativePlayer();}catch(error){playerError=errorText(error);render();}});
}

function resetPlayback() {stopPlayerPolling();playerGeneration++;playerId='';playerLoading=false;playerError='';playerState={duration:0,currentTime:0,playing:false};void releaseNativePlayback().catch(()=>{});audioPosition=0;audioWasPlaying=false;const audio=document.querySelector<HTMLAudioElement>('#audio');if(audio){audio.pause();audio.currentTime=0;}}
async function mayReplaceRecord(discardRecording=true) {if(pendingRecording){if(discardRecording){if(!await discardPendingRecording())return false;}else if(!await confirmAction(tr('Удалить записанное аудио?'),tr('Эта запись ещё не добавлена в архив и будет потеряна.'),tr('Продолжить без записи'),false))return false;}return !text||resultSaved||await confirmAction(tr("Неполный текст не сохранён"),tr("Скопируйте или экспортируйте текст, чтобы сохранить его. При переходе к другой записи этот результат будет потерян."),tr("Продолжить без текста"),false);}
async function mayLeaveSettings() {
  if(settingsSaving||!await mayLeaveRules())return false;
  if(!settingsDraft&&!hasTransferDraft())return true;
  if(!await confirmAction(tr("Изменения не применены"),tr("Примените настройки перед уходом или продолжите без этих изменений."),tr("Продолжить без изменений"),false))return false;
  settingsDraft=null;settingsError='';discardTransferDraft();return true;
}
async function changeView(target:View, settingsTarget?:SettingsSection):Promise<boolean|void> {
  if(recordingPhase!=='idle')return;
  // Desktop keeps the record list in the sidebar, so "history" means focusing its search.
  if(target==='history'&&!platform.mobile){if(view!=='work'&&!await changeView('work'))return;if(sidebarCollapsed)setSidebar(false);document.querySelector<HTMLInputElement>('#history-search')?.focus();return;}
  if(view==='rules'&&target!=='rules'&&!await mayLeaveRules())return;
  if(target==='models'&&!platform.localRecognition)return;
  if((view==='settings'||view==='devices')&&target!==view&&!await mayLeaveSettings())return;
  if(target==='settings') {
    if(view==='settings'&&settingsTarget&&settingsTarget!==settingsSection&&!await mayLeaveSettings())return;
    if(view!=='settings')settingsSection=settingsTarget||((platform.mobile||window.innerWidth<=760)?null:'general');
    else if(settingsTarget)settingsSection=settingsTarget;
  }
  view=target;status='';if(window.innerWidth<=760&&!platform.mobile)sidebarCollapsed=true;render();if(target==='rules')ensureRules();if(target==='archive')void refreshLoose();return true;
}
function bindRecordActions() {
  document.querySelectorAll<HTMLElement>('[data-history]').forEach(element=>element.onclick=()=>loadHistory(element.dataset.history!));
  document.querySelectorAll<HTMLElement>('[data-delete]').forEach(element=>element.onclick=()=>deleteEntry(element.dataset.delete!));
  document.querySelectorAll<HTMLElement>('[data-rename]').forEach(element=>element.onclick=()=>renameEntry(element.dataset.rename!));
}
function bind() {
  bindOperationCancel();
  document.querySelector('#start-recording')?.addEventListener('click',startRecording);
  document.querySelector('#stop-recording')?.addEventListener('click',stopRecording);
  document.querySelector('#pause-recording')?.addEventListener('click',()=>{void toggleRecordingPause();});
  document.querySelector('#cancel-recording')?.addEventListener('click',cancelRecording);
  document.querySelector('#retry-recording')?.addEventListener('click',()=>{if(pendingRecording&&!busy)void importSelection(pendingRecording);});
  document.querySelector('#discard-recording')?.addEventListener('click',async()=>{if(await discardPendingRecording())render();});
  document.querySelectorAll<HTMLElement>('[data-view]').forEach(element=>element.onclick=()=>changeView(element.dataset.view as View,isSettingsSection(element.dataset.settingsTarget)?element.dataset.settingsTarget:undefined));
  document.querySelectorAll('[data-choose]').forEach(element=>element.addEventListener('click',()=>choose()));
  document.querySelector('#new')?.addEventListener('click',newRecord);
  document.querySelector('#quick-record')?.addEventListener('click',()=>{void quickRecord();});
  document.querySelectorAll<HTMLElement>('[data-back]').forEach(element=>element.onclick=()=>changeView(element.dataset.back as View));
  document.querySelector('#copy-transcript')?.addEventListener('click',()=>copyText(text));
  document.querySelector('#run')?.addEventListener('click',run);
  document.querySelectorAll('button[data-theme]').forEach(element=>element.addEventListener('click',()=>{theme=theme==='dark'?'light':'dark';document.documentElement.dataset.theme=theme;localStorage.setItem('hearing-theme',theme);render();}));
  document.querySelector('#collapse')?.addEventListener('click',()=>setSidebar(true));document.querySelector('#expand')?.addEventListener('click',()=>setSidebar(false));
  document.querySelectorAll<HTMLElement>('[data-provider]').forEach(element=>element.onclick=()=>{const provider=element.dataset.provider as RecognitionConfig['provider'];if(provider===configuration.provider)return;chooseConfiguration(provider==='local'?{provider,model:localChoice,mode:'local'}:{provider,model:cloudChoice,mode:cloudModels.find(model=>model.id===cloudChoice)?.preferredMode||'streaming'});if(provider==='openrouter'&&!cloudModels.length)loadCloudModels();});
  document.querySelector<HTMLSelectElement>('#local-model')?.addEventListener('change',event=>{chooseConfiguration({provider:'local',model:(event.target as HTMLSelectElement).value,mode:'local'});});
  document.querySelectorAll<HTMLElement>('[data-select]').forEach(element=>element.onclick=()=>chooseConfiguration({provider:'local',model:element.dataset.select!,mode:'local'}));
  bindCatalog();bindPostprocess();bindNativePlayer();
  document.querySelector('#process-shortcut')?.addEventListener('click',()=>{const controls=document.querySelector<HTMLElement>('.postprocess-controls');if(controls){controls.scrollIntoView({behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'auto':'smooth',block:'start'});controls.focus({preventScroll:true});}});
  document.querySelectorAll<HTMLElement>('[data-output-tab]').forEach(element=>element.onclick=()=>{outputTab=element.dataset.outputTab as typeof outputTab;render();});
  document.querySelector('#record-share')?.addEventListener('click',openRecordActions);
  document.querySelectorAll<HTMLElement>('[data-download]').forEach(element=>element.onclick=()=>download(element.dataset.download!));
  bindRecordActions();
  document.querySelector<HTMLInputElement>('#history-search')?.addEventListener('input',event=>{search=(event.target as HTMLInputElement).value;const results=document.querySelector('#history-results');if(results){results.innerHTML=historyResults();bindRecordActions();results.querySelector('#retry-history')?.addEventListener('click',()=>refreshHistory());}});
  document.querySelector('#clear')?.addEventListener('click',clearHistory);
  document.querySelector('#retry-history')?.addEventListener('click',()=>refreshHistory());
  document.querySelector('#retry-runtime')?.addEventListener('click',()=>refreshRuntime());
  document.querySelector('#copy-install')?.addEventListener('click',()=>copyText(platform.os==='linux'?'sudo apt install ffmpeg':'brew install ffmpeg whisper.cpp'));
  document.querySelector('#restore')?.addEventListener('click',()=>{if(previousResult){text=previousResult.text;resultSaved=previousResult.saved;previousResult=null;render();}});
  document.querySelectorAll<HTMLAnchorElement>('a[href^="https://"]').forEach(anchor=>anchor.onclick=event=>{event.preventDefault();openExternal(anchor.href).catch(error=>notice(errorText(error),true));});
  bindSettings();bindUpdates();bindTransfers(active);bindArchive();
}
function setSidebar(collapsed:boolean) {sidebarCollapsed=collapsed;localStorage.setItem('hearfolio-sidebar',collapsed?'collapsed':'expanded');render();}
let recordActionsDialog:HTMLDialogElement|null=null;
function openRecordActions(){
  if(!active||recordActionsDialog?.open)return;
  const entry=active,transcript=text,trigger=document.querySelector<HTMLElement>('#record-share');
  const dialog=document.createElement('dialog');recordActionsDialog=dialog;dialog.className='record-actions-dialog';dialog.id='record-actions-dialog';trigger?.setAttribute('aria-expanded','true');dialog.setAttribute('aria-labelledby','record-actions-title');
  const action=(name:string,label:string,glyph:string)=>`<button type="button" data-record-action="${name}">${icon(glyph)}<span>${tr(label)}</span></button>`;
  dialog.innerHTML=`<div class="record-actions-heading"><h2 id="record-actions-title">${tr('Поделиться')}</h2><button type="button" class="icon-button" data-actions-close aria-label="${tr('Закрыть')}">${icon('close')}</button></div><div class="record-actions-group">${action('share-audio','Поделиться аудио','share')}${transcript.trim()?action('share-text','Поделиться текстом','file'):''}${!busy?action('device','На другое устройство','devices'):''}</div><div class="record-actions-group">${action('save-audio','Сохранить аудио','download')}${transcript.trim()?action('save-text','Сохранить текст','download')+action('copy-text','Копировать текст','copy'):''}</div>`;
  dialog.querySelector('[data-actions-close]')?.addEventListener('click',()=>dialog.close());
  dialog.querySelectorAll<HTMLElement>('[data-record-action]').forEach(button=>button.onclick=()=>{const choice=button.dataset.recordAction;dialog.close();if(choice==='device'){shareRecordingToDevice(entry);return;}const operation=choice==='share-audio'?shareAudio(entry):choice==='save-audio'?saveAudio(entry):choice==='share-text'?shareText(entry.name,transcript):choice==='save-text'?exportText(recordNameParts(entry.name,entry.inputPath).title.replace(/[\/\\:\u0000-\u001f]/g,'-')+'.txt',transcript):copyText(transcript);void operation.catch(error=>notice(errorText(error),true));});
  dialog.addEventListener('click',event=>{if(event.target===dialog){const rect=dialog.getBoundingClientRect();if(event.clientX<rect.left||event.clientX>rect.right||event.clientY<rect.top||event.clientY>rect.bottom)dialog.close();}});
  dialog.addEventListener('close',()=>{dialog.remove();if(recordActionsDialog===dialog)recordActionsDialog=null;trigger?.setAttribute('aria-expanded','false');if(trigger?.isConnected)trigger.focus({preventScroll:true});});
  document.body.append(dialog);dialog.showModal();dialog.querySelector<HTMLElement>('[data-record-action]')?.focus();
}
async function copyText(value:string) {try{await navigator.clipboard.writeText(value);notice(tr("Скопировано"));}catch(error){notice(`${tr("Не удалось скопировать текст: ")}${errorText(error)}`,true);}}
function readSettingsDraft(form:HTMLFormElement):SettingsDraft {return mergeSettingsDraft(new FormData(form),settingsDraft||initialSettingsDraft(settings));}
function bindSettings() {
  document.querySelectorAll<HTMLElement>('[data-settings-section]').forEach(button=>button.addEventListener('click',async()=>{
    const target=button.dataset.settingsSection;if(!isSettingsSection(target)||target===settingsSection||settingsSaving)return;
    if(!await mayLeaveSettings())return;settingsSection=target;render();document.querySelector<HTMLElement>('#settings-category-title')?.focus({preventScroll:true});
  }));
  document.querySelector('[data-settings-back]')?.addEventListener('click',async()=>{if(!await mayLeaveSettings())return;const previous=settingsSection;settingsSection=null;render();document.querySelector<HTMLElement>(`[data-settings-section="${previous}"]`)?.focus({preventScroll:true});});
  const form=document.querySelector<HTMLFormElement>('#settings-form');
  form?.addEventListener('input',()=>{
    const draft=readSettingsDraft(form);settingsDraft=hasSettingsChanges(draft,settings)?draft:null;settingsError='';
    const button=form.querySelector<HTMLButtonElement>('[type=submit]');if(button)button.disabled=busy||settingsSaving||!settingsDraft;
    const hint=form.querySelector('.settings-actions span');if(hint)hint.textContent=settingsDraft?settingsLabel('Есть несохранённые изменения','Unsaved changes'):settingsLabel('Все изменения сохранены','All changes saved');
    document.querySelector('.settings-error')?.remove();
  });
  form?.addEventListener('submit',async event=>{event.preventDefault();await applySettings();});
  document.querySelector('#remove-token')?.addEventListener('click',async()=>{
    if(busy||settingsSaving)return;
    if(!await confirmAction(tr("Удалить API token?"),tr("Для распознавания через OpenRouter потребуется подключить токен снова."),tr("Удалить токен")))return;
    settingsSaving=true;settingsError='';render();
    const saved=await persist('');settingsSaving=false;if(saved){cloudModels=[];catalogError='';}else settingsError=settingsLabel('Не удалось удалить токен. Повторите попытку.','Could not remove the token. Try again.');render();
  });
  document.querySelector('#choose-storage')?.addEventListener('click',changeStorage);
  document.querySelector('#open-battery-settings')?.addEventListener('click',()=>{void openAndroidBatterySettings().catch(error=>notice(errorText(error),true));});
}
async function applySettings():Promise<boolean> {
  if(busy||settingsSaving)return false;
  const form=document.querySelector<HTMLFormElement>('#settings-form');if(!form||!form.reportValidity())return false;
  const draft=readSettingsDraft(form),connectionChanged=draft.url.trim()!==settings.openrouterUrl||Boolean(draft.token.trim());
  settingsDraft=draft;settingsSaving=true;settingsError='';render();
  const saved=await persist(draft.token.trim()||undefined,{language:draft.language,openrouterUrl:draft.url.trim(),chunkSeconds:draft.chunkSeconds});
  settingsSaving=false;
  if(!saved){settingsError=settingsLabel('Настройки не сохранены. Ваши изменения остаются здесь; попробуйте ещё раз.','Settings were not saved. Your changes are still here; try again.');render();return false;}
  settingsDraft=null;if(connectionChanged){cloudModels=readAudioCatalog(localStorage,settings.openrouterUrl);catalogError='';void loadCloudModels();}render();notice(tr("Настройки применены"));return true;
}
async function persist(token?:string,snapshot:{language:LanguagePreference;openrouterUrl:string;chunkSeconds?:number}={language:settings.language,openrouterUrl:settings.openrouterUrl,chunkSeconds:settings.chunkSeconds}):Promise<boolean> {
  const operation=settingsSaveQueue.then(async()=>{try{settings=await call<Settings>('save_settings',{settings:snapshot,...(token!==undefined?{token}:{})});applyLanguagePreference();return true;}catch(error){notice(`${tr("Не удалось применить настройки: ")}${errorText(error)}`,true);return false;}});
  settingsSaveQueue=operation;return operation;
}
async function changeStorage() {
  if(busy||!platform.customStorage)return;
  try{
    const parent=await pickStorageParent(settings.storageParent);if(!parent||parent===settings.storageParent)return;
    beginOperation('storage',tr("Переносим архив"));preservePlayback();render();
    settings=await call<Settings>('set_storage_parent',{parent});
    history=sortHistory(await call<Entry[]>('list_history'));
    if(active){const detail=await call<{entry:Entry;text:string}>('get_history',{id:active.id});active=detail.entry;}
    const statuses=await call<{id:string;installed:boolean}[]>('model_status');models.forEach(model=>model.installed=statuses.find(item=>item.id===model.id)?.installed||false);
    notice(tr("Архив перенесён"));
  }catch(error){notice(`${tr("Не удалось перенести архив: ")}${errorText(error)}`,true);}
  finally{endOperation();render();}
}
function normalizeCatalogConfiguration(candidate:RecognitionConfig):RecognitionConfig {
  const model=candidate.provider==='openrouter'?cloudModels.find(item=>item.id===candidate.model):null;
  return model&&candidate.mode!==model.preferredMode?{...candidate,mode:model.preferredMode}:candidate;
}
function restoreConfiguration(entry?:Entry|null) {
  const legacyModel=entry?.model?.replace(/^openrouter\//,'');
  configuration={...(entry?.configuration||(legacyModel?{provider:legacyModel.startsWith('whisper-')||legacyModel==='nemotron-3.5'?'local':'openrouter',model:legacyModel,mode:legacyModel.startsWith('whisper-')||legacyModel==='nemotron-3.5'?'local':'streaming'}:settings.lastConfiguration))} as RecognitionConfig;
  if(!platform.localRecognition&&configuration.provider==='local')configuration={provider:'openrouter',model:cloudChoice||'google/gemini-2.5-flash',mode:'streaming'};
  configuration=normalizeCatalogConfiguration(configuration);
  if(configuration.provider==='local')localChoice=configuration.model;else cloudChoice=configuration.model;
  catalogOpen=false;catalogSearch='';
  if(configuration.provider==='openrouter'&&!cloudModels.length)void loadCloudModels();
}
function chooseConfiguration(next:RecognitionConfig) {
  if(busy||(!platform.localRecognition&&next.provider==='local'))return;
  configuration={...next};if(next.provider==='local')localChoice=next.model;else cloudChoice=next.model;
  if(active&&next.model.trim()){const id=active.id,snapshot={...next};active={...active,configuration:snapshot};history=history.map(entry=>entry.id===id?{...entry,configuration:snapshot}:entry);configurationSaveQueue=configurationSaveQueue.then(()=>call('update_record_configuration',{id,configuration:snapshot})).catch(error=>notice(errorText(error),true));}
  render();
}
function bindCatalogOptions() {
  document.querySelectorAll<HTMLElement>('[data-cloud-model]').forEach(element=>element.onclick=()=>{const model=cloudModels.find(item=>item.id===element.dataset.cloudModel);if(model){catalogOpen=false;chooseConfiguration({provider:'openrouter',model:model.id,mode:model.preferredMode});document.querySelector<HTMLButtonElement>('#cloud-picker-toggle')?.focus();}});
}
function closeCatalog(){catalogOpen=false;render();document.querySelector<HTMLButtonElement>('#cloud-picker-toggle')?.focus();}
function bindCatalog() {
  const backdrop=document.querySelector('#catalog-backdrop');
  backdrop?.addEventListener('click',event=>{if(event.target===backdrop)closeCatalog();});
  if(platform.mobile&&catalogOpen){
    const dialog=document.querySelector<HTMLElement>('.model-catalog');dialog?.setAttribute('aria-modal','true');
    dialog?.addEventListener('keydown',event=>{if(event.key!=='Tab')return;const items=Array.from(dialog.querySelectorAll<HTMLElement>('button:not(:disabled),input'));const first=items[0],last=items[items.length-1];if(event.shiftKey&&document.activeElement===first){event.preventDefault();last?.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first?.focus();}});
  }
  document.querySelector('#cloud-picker-toggle')?.addEventListener('click',()=>{catalogOpen=!catalogOpen;render();if(catalogOpen){if(!platform.mobile&&window.innerWidth>760)document.querySelector<HTMLInputElement>('#catalog-search')?.focus();if(!cloudModels.length)void loadCloudModels();}});
  document.querySelector('#catalog-close')?.addEventListener('click',closeCatalog);
  document.querySelector('#open-catalog')?.addEventListener('click',()=>{catalogOpen=true;render();if(!platform.mobile&&window.innerWidth>760)document.querySelector<HTMLInputElement>('#catalog-search')?.focus();if(!cloudModels.length)void loadCloudModels();});
  document.querySelector('#catalog-refresh')?.addEventListener('click',()=>loadCloudModels());
  document.querySelector('#retry-catalog')?.addEventListener('click',()=>loadCloudModels());
  document.querySelector<HTMLInputElement>('#catalog-search')?.addEventListener('input',event=>{catalogSearch=(event.target as HTMLInputElement).value;const matches=cloudModels.filter(model=>`${model.name} ${model.id}`.toLocaleLowerCase().includes(catalogSearch.toLocaleLowerCase()));document.querySelector('#catalog-results')!.innerHTML=catalogResults(matches);bindCatalogOptions();});
  bindCatalogOptions();
}
async function loadCloudModels() {
  if(cloudLoading)return;
  const url=settings.openrouterUrl;
  cloudLoading=true;catalogError='';render();
  try{
    const loaded=normalizeAudioCatalog(await call<CloudModel[]>('list_openrouter_models'));
    if(url!==settings.openrouterUrl)return;
    cloudModels=loaded;saveAudioCatalog(localStorage,url,loaded);
    const normalized=normalizeCatalogConfiguration(configuration);
    if(normalized.mode!==configuration.mode){if(!busy)chooseConfiguration(normalized);else if(progress?.kind!=='transcribe')configuration=normalized;}
  }
  catch(error){if(url===settings.openrouterUrl)catalogError=errorText(error);}
  finally{cloudLoading=false;render();if(url!==settings.openrouterUrl)void loadCloudModels();else if(catalogOpen&&!platform.mobile&&window.innerWidth>760)document.querySelector<HTMLInputElement>('#catalog-search')?.focus();}
}
function sortHistory(entries:Entry[]) {return entries.sort((left,right)=>right.createdAt-left.createdAt);}
async function refreshAfterTransfer() {
  await refreshHistory();
  // A running job owns the open record; refreshing it would replace streamed text and close the model picker.
  if(active&&!busy&&!catalogOpen&&(resultSaved||!text)) {
    try {
      const detail=await call<{entry:Entry;text:string}>('get_history',{id:active.id});
      active=detail.entry;restoreConfiguration(detail.entry);text=detail.text;
      resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;render();
    } catch(error) {notice(errorText(error),true);}
  }
}
async function refreshHistory() {try{history=sortHistory(await call<Entry[]>('list_history'));historyError='';}catch(error){historyError=errorText(error);}loading=false;render();}
async function refreshRuntime() {
  if(!platform.localRecognition){runtime={ffmpeg:false,whisper:false,nemo:false};runtimeLoaded=true;render();return;}
  const results=await Promise.allSettled([call<typeof runtime>('runtime_status'),call<{id:string;installed:boolean}[]>('model_status')]);
  if(results[0].status==='fulfilled'){runtime=results[0].value;runtimeLoaded=true;}else notice(`${tr("Не удалось проверить инструменты: ")}${errorText(results[0].reason)}`,true);
  if(results[1].status==='fulfilled'){const statuses=results[1].value;models.forEach(model=>model.installed=statuses.find(item=>item.id===model.id)?.installed||false);}else notice(`${tr("Не удалось проверить модели: ")}${errorText(results[1].reason)}`,true);
  render();
}
async function newRecord():Promise<boolean> {
  if(busy||!await mayLeaveSettings()||!await mayReplaceRecord())return false;
  resetPlayback();settingsDraft=null;active=null;restoreConfiguration();text='';outputTab='transcript';resultSaved=false;previousResult=null;status='';view='work';render();return true;
}
async function quickRecord(){if(busy||recordingPhase!=='idle')return;if(active&&!await newRecord())return;await startRecording();}
async function discardPendingRecording():Promise<boolean> {
  if(!pendingRecording)return true;
  if(!await confirmAction(tr('Удалить записанное аудио?'),tr('Эта запись ещё не добавлена в архив и будет потеряна.'),tr('Удалить запись')))return false;
  busy=true;render();
  try{await discardAudioRecording(pendingRecording);pendingRecording=null;return true;}
  catch(error){notice(errorText(error),true);return false;}
  finally{busy=false;render();}
}
function clearRecordingTimer(){if(recordingTimer)clearInterval(recordingTimer);recordingTimer=null;}
function resetRecorderUi(){
  clearRecordingTimer();recordingPhase='idle';recordingStarted=0;externalRecording=false;busy=false;
  nativeRecordingSeconds=null;nativeRecordingMillis=null;nativeRecorderClock.reset();restoreNativeRecordingFlag(false);
}
async function startRecording(){
  if(busy||active||!platform.audioRecording||!await mayLeaveSettings()||!await mayReplaceRecord())return;
  if(busy||active)return;
  nativeCaptureGeneration++;nativeRecordingOrder.beginStart();lastNativeRecordingResult='';nativeRecordingSeconds=null;nativeRecordingMillis=null;nativeRecordingPath='';recordingPausedAt=0;recordingPausedMillis=0;nativeRecorderClock.reset();recorderEnvelope.reset();settingsDraft=null;view='work';recordingPhase='starting';busy=true;status='';recordingStarted=0;render();
  try{await startAudioRecording();nativeRecordingOrder.confirmStart();recordingStarted=Date.now();recordingPhase='recording';recordingTimer=setInterval(()=>{const timer=document.querySelector('#recording-duration');if(timer)timer.textContent=recordingDuration();},100);await syncNativeRecording(true);render();}
  catch(error){nativeRecordingOrder.abortStart();recordingPhase='idle';busy=false;if(platform.os==='android'&&!preview)await syncNativeRecording(true);render();if(recordingPhase==='idle')notice(errorText(error),true);}
}
function recordingName(selected:AudioSelection,started=recordingStarted||Date.now()):string {
  const date=new Date(started),pad=(value:number)=>String(value).padStart(2,'0');
  const day=currentLanguage()==='ru'?`${pad(date.getDate())}.${pad(date.getMonth()+1)}.${date.getFullYear()}`:`${date.getFullYear()}-${pad(date.getMonth()+1)}-${pad(date.getDate())}`;
  const extension=(selected.name||selected.path).match(/\.(wav|m4a|webm|ogg|mp3|aac)$/i)?.[1]?.toLowerCase()||(platform.os==='android'?'m4a':'wav');
  return `${tr('Запись')} ${day} ${pad(date.getHours())}.${pad(date.getMinutes())}.${extension}`;
}
async function toggleRecordingPause(){
  if(!['recording','paused'].includes(recordingPhase)||recordingControlPending)return;
  const paused=recordingPhase==='paused';nativeCaptureGeneration++;recordingControlPending=true;render();
  try{
    if(externalRecording)await call('recording_quick_control',{action:paused?'resume':'pause',path:nativeRecordingPath||undefined});
    else if(paused)await resumeAudioRecording();else await pauseAudioRecording();
    if(platform.os==='android'&&!preview){await syncNativeRecording(true);}
    else{if(paused){if(recordingPausedAt)recordingPausedMillis+=Date.now()-recordingPausedAt;recordingPausedAt=0;}else recordingPausedAt=Date.now();recordingPhase=paused?'recording':'paused';}
  }catch(error){
    if(platform.os==='android'&&!preview)await syncNativeRecording(true);
    if(recordingPhase===(paused?'paused':'recording'))notice(errorText(error),true);
  }
  finally{recordingControlPending=false;render();}
}
async function stopRecording(){
  if(!['recording','paused'].includes(recordingPhase)||recordingControlPending)return;
  nativeCaptureGeneration++;recordingControlPending=true;clearRecordingTimer();recordingPhase='stopping';render();
  try{
    if(externalRecording){
      try{await call('recording_quick_control',{action:'stop',path:nativeRecordingPath||undefined});await finishQuickRecording();}
      catch(error){await syncNativeRecording(true);if(['recording','paused'].includes(recordingPhase))notice(errorText(error),true);}
      return;
    }
    pendingRecording=await stopAudioRecording();recordingPhase='idle';
    if(!pendingRecording)throw new Error('errors.recordingEmpty');
    pendingRecording={...pendingRecording,name:recordingName(pendingRecording)};
    await importSelection(pendingRecording);
  }catch(error){
    if(platform.os==='android'&&!preview){await syncNativeRecording(true);render();if(['recording','paused'].includes(recordingPhase))notice(errorText(error),true);}
    else{await cancelAudioRecording().catch(()=>{});recordingPhase='idle';busy=false;render();notice(errorText(error),true);}
  }finally{recordingControlPending=false;render();}
}
async function cancelRecording(){
  if(!['recording','paused'].includes(recordingPhase)||recordingControlPending)return;
  nativeCaptureGeneration++;recordingControlPending=true;clearRecordingTimer();recordingPhase='cancelling';render();
  try{if(externalRecording)await call('recording_quick_control',{action:'cancel',path:nativeRecordingPath||undefined});else await cancelAudioRecording();
    if(platform.os==='android'&&!preview)await syncNativeRecording(true);
    else resetRecorderUi();
  }catch(error){if(platform.os==='android'&&!preview)await syncNativeRecording(true);else resetRecorderUi();if(!['idle'].includes(recordingPhase))notice(errorText(error),true);}
  finally{recordingControlPending=false;render();}
}
async function importSelection(selected:AudioSelection):Promise<boolean>{
  beginOperation('import',tr('Добавляем запись'));settingsDraft=null;view='work';render();
  let imported=false;
  try{
    const entry=await call<Entry>('import_audio',{path:selected.path,...(selected.name?{name:selected.name}:{}),configuration:{...(configuration.model.trim()?configuration:settings.lastConfiguration)}});
    // Native recovery can report this same archived recording after the user
    // has opened a new capture. Share its receipt with the recovery handler.
    if(platform.os==='android')markOperationHandled(`recording:${entry.id}`);
    imported=true;pendingRecording=null;resetPlayback();active=entry;restoreConfiguration(entry);text='';outputTab='transcript';resultSaved=false;previousResult=null;
    const refreshed=await Promise.allSettled([call<Settings>('get_settings'),call<Entry[]>('list_history')]);
    if(refreshed[0].status==='fulfilled')settings=refreshed[0].value;
    if(refreshed[1].status==='fulfilled')history=sortHistory(refreshed[1].value);
    else notice(errorText(refreshed[1].reason),true);
  }catch(error){notice(`${tr('Не удалось добавить запись: ')}${errorText(error)}`,true);}
  finally{endOperation();render();}
  return imported;
}
async function choose(path?:string) {
  if(busy||pickingAudio||platform.os==='unknown')return;
  if(active&&!await newRecord())return;
  if(!await mayLeaveSettings()||!await mayReplaceRecord())return;
  pickingAudio=true;
  try{const selected=path?{path}:await pickAudio();if(selected&&!busy&&!active)await importSelection(selected);}
  catch(error){notice(`${tr('Не удалось добавить запись: ')}${errorText(error)}`,true);}
  finally{pickingAudio=false;}
}
async function loadHistory(id:string) {
  if(active?.id===id){if(!await mayLeaveSettings())return;settingsDraft=null;view='work';render();return;}
  if(busy){notice(tr('Дождитесь окончания текущей задачи.'));return;}
  if(!await mayLeaveSettings())return;
  if(!await mayReplaceRecord())return;
  try {const detail=await call<{entry:Entry;text:string;warning?:string|null}>('get_history',{id});resetPlayback();active=detail.entry;restoreConfiguration(detail.entry);text=detail.text;outputTab='transcript';ensureRules();resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;settingsDraft=null;view='work';resume=null;void refreshResume(id);status=detail.warning?errorText(detail.warning):'';isError=Boolean(detail.warning);render();}
  catch(error) {notice(`${tr("Не удалось открыть запись: ")}${errorText(error)}`,true);}
}
async function renameEntry(id:string) {
  if(busy||document.querySelector('dialog[open]'))return;
  const entry=history.find(item=>item.id===id);if(!entry)return;
  const parts=recordNameParts(entry.name,entry.inputPath);
  const dialog=document.createElement('dialog');dialog.className='confirmation rename-dialog';
  dialog.setAttribute('aria-labelledby','rename-title');
  dialog.innerHTML=`${tr("<form><h2 id=\"rename-title\">Переименовать запись</h2><label class=\"field\"><span>Название</span><input name=\"record-name\" value=\"")}${esc(parts.title)}${tr("\" required autocomplete=\"off\" aria-describedby=\"rename-error\">" )}${parts.extension?`<span class="rename-extension" aria-label="${tr('Формат аудио')}">${esc(parts.extension)}</span>`:''}${tr("</label><p id=\"rename-error\" class=\"rename-error\" role=\"alert\" hidden></p><div class=\"rename-actions\"><button type=\"button\" class=\"secondary\" data-cancel>Отмена</button><button type=\"submit\" class=\"primary\">Сохранить</button></div></form>")}`;
  document.body.append(dialog);
  const input=dialog.querySelector<HTMLInputElement>('input')!,form=dialog.querySelector<HTMLFormElement>('form')!;
  input.setAttribute('aria-label',tr('Название'));
  const renameError=dialog.querySelector<HTMLElement>('#rename-error')!,saveButton=dialog.querySelector<HTMLButtonElement>('[type=submit]')!,cancelButton=dialog.querySelector<HTMLButtonElement>('[data-cancel]')!;
  input.maxLength=200-parts.extension.length;
  let saving=false;
  const previousFocus=document.activeElement as HTMLElement|null;
  const close=()=>{dialog.close();dialog.remove();if(previousFocus?.isConnected)previousFocus.focus({preventScroll:true});};
  input.addEventListener('input',()=>{input.setCustomValidity('');renameError.hidden=true;});
  cancelButton.onclick=()=>{if(!saving)close();};
  dialog.addEventListener('cancel',event=>{event.preventDefault();if(!saving)close();});
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(saving)return;
    const name=renamedRecordTitle(input.value,parts.extension);input.setCustomValidity(name?'':tr("Введите название записи"));
    if(!form.reportValidity())return;
    if(name===entry.name){close();return;}
    saving=true;saveButton.disabled=true;cancelButton.disabled=true;input.disabled=true;saveButton.textContent=tr("Сохраняем…");
    try{
      const updated=await call<Entry>('rename_history',{id,name});
      history=history.map(item=>item.id===id?updated:item);
      if(active?.id===id)active={...active,name:updated.name};
      const scroll=document.querySelector<HTMLElement>('.transcript-scroll,.page-scroll'),scrollTop=scroll?.scrollTop||0;
      close();render();
      const restoredScroll=document.querySelector<HTMLElement>('.transcript-scroll,.page-scroll');if(restoredScroll)restoredScroll.scrollTop=scrollTop;
      if(view==='history'&&!platform.mobile&&window.innerWidth>760)document.querySelector<HTMLInputElement>('#history-search')?.focus();
      notice(tr("Название изменено"));
    }catch(error){
      renameError.textContent=`${tr("Не удалось переименовать запись: ")}${errorText(error)}`;renameError.hidden=false;
      saving=false;saveButton.disabled=false;cancelButton.disabled=false;input.disabled=false;saveButton.textContent=tr("Сохранить");input.focus();
    }
  });
  input.setAttribute('autofocus','');dialog.showModal();input.focus();input.select();
}
async function deleteEntry(id:string) {
  if(busy)return;const entry=history.find(item=>item.id===id);if(!entry)return;
  if(active?.id===id&&text&&!resultSaved&&!await mayReplaceRecord(false))return;
  if(!await confirmAction(tr("Удалить запись?"),`«${entry.name}${tr("» и её расшифровка будут удалены из архива. Исходный файл останется на месте.")}`,tr("Удалить запись")))return;
  try{await call('delete_history',{id});history=history.filter(item=>item.id!==id);if(active?.id===id){resetPlayback();active=null;text='';outputTab='transcript';resultSaved=false;previousResult=null;if(platform.mobile)view='history';}render();}
  catch(error){notice(`${tr("Не удалось удалить запись: ")}${errorText(error)}`,true);}
}
async function clearHistory() {
  if(busy||!await confirmAction(tr("Очистить архив?"),`${tr("Будут удалены все записи из архива (")}${recordCount(history.length)}${tr(") и их расшифровки. Исходные файлы останутся на месте.")}`,tr("Очистить архив")))return;
  try{await call('clear_history');resetPlayback();history=[];active=null;text='';outputTab='transcript';resultSaved=false;previousResult=null;render();}
  catch(error){notice(`${tr("Не удалось очистить архив: ")}${errorText(error)}`,true);}
}
async function download(id:string) {
  if(busy)return;beginOperation('download',tr("Скачиваем модель"));render();
  try{await call('download_model',{id});const statuses=await call<{id:string;installed:boolean}[]>('model_status');models.forEach(model=>model.installed=statuses.find(item=>item.id===model.id)?.installed||false);}
  catch(error){endOperation();notice(`${tr("Не удалось скачать модель: ")}${errorText(error)}`,true);}
  finally{endOperation();render();}
}
async function run() {
  if(!active||busy||readiness())return;
  const id=active.id,beforeAttempt={text,saved:resultSaved};
  if(text&&resultSaved)previousResult={text,saved:true};
  receivedPartial=false;outputTab='transcript';
  beginOperation('transcribe',tr("Подготавливаем запись"));startReveal(0);render();
  try {
    await settingsSaveQueue;await configurationSaveQueue;
    text=await call<string>('transcribe',{id,configuration:{...configuration}});finishReveal();
    const detail=await call<{entry:Entry;text:string;warning?:string|null}>('get_history',{id});active=detail.entry;resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;
    settings=await call<Settings>('get_settings');
    history=sortHistory(await call<Entry[]>('list_history'));resume=null;
  } catch(error) {
    void refreshResume(id);
    if(!receivedPartial){text=beforeAttempt.text;resultSaved=beforeAttempt.saved;if(resultSaved)previousResult=null;stopReveal();}else finishReveal();
    status=`${tr("Не удалось завершить распознавание: ")}${errorText(error)}`;isError=true;
  } finally {ensureRules();try{settings=await call<Settings>('get_settings');}catch{/* Keep known defaults when settings cannot be refreshed. */}endOperation();render();}
}
function confirmAction(heading:string,message:string,action:string,danger=true):Promise<boolean> {
  if(document.querySelector('dialog[open]'))return Promise.resolve(false);
  return new Promise(resolve=>{
    const dialog=document.createElement('dialog');dialog.className='confirmation';dialog.setAttribute('aria-labelledby','confirmation-title');dialog.setAttribute('aria-describedby','confirmation-description');
    dialog.innerHTML=`<h2 id="confirmation-title">${esc(heading)}</h2><p id="confirmation-description">${esc(message)}${tr("</p><div><button class=\"secondary\" data-cancel>Отмена</button><button class=\"")}${danger?'destructive':'primary'}" data-confirm>${esc(action)}</button></div>`;
    document.body.append(dialog);dialog.showModal();
    const finish=(value:boolean)=>{dialog.close();dialog.remove();resolve(value);};
    dialog.querySelector('[data-cancel]')?.addEventListener('click',()=>finish(false));dialog.querySelector('[data-confirm]')?.addEventListener('click',()=>finish(true));dialog.addEventListener('cancel',event=>{event.preventDefault();finish(false);});dialog.querySelector<HTMLButtonElement>('[data-cancel]')?.focus();
  });
}

setupPostprocess({entry:()=>active,transcript:()=>text,saved:()=>resultSaved,busy:()=>busy,render,notice,begin:()=>beginOperation('postprocess',tr('Обрабатываем текст…')),end:endOperation,confirm:confirmAction,updated:entry=>{active=entry;history=history.map(item=>item.id===entry.id?entry:item);},showResults:()=>{outputTab='results';},configure:()=>{void changeView('settings','recognition');},copy:copyText,hasToken:()=>settings.hasToken});
render();
async function initialize() {
  const initial=await Promise.allSettled([loadRuntimePlatform(),call<Settings>('get_settings'),appVersion()]);
  if(initial[0].status==='fulfilled')platform=initial[0].value;else notice(errorText(initial[0].reason),true);
  if(initial[1].status==='fulfilled'){settings=initial[1].value;applyLanguagePreference();}else notice(`${tr('Не удалось загрузить настройки: ')}${errorText(initial[1].reason)}`,true);
  if(initial[2].status==='fulfilled')applicationVersion=initial[2].value;
  booted=true;if(platform.mobile)view='history';
  if(['macos','linux','windows'].includes(platform.os))subscribeAudioDrop(paths=>{if(paths.length){if(paths.length>1)notice(tr("Добавляем первый файл. Остальные можно добавить по одному."));choose(paths[0]);}}).catch(error=>notice(errorText(error),true));
  cloudModels=readAudioCatalog(localStorage,settings.openrouterUrl);
  restoreConfiguration(active);render();
  if(configuration.provider==='openrouter'&&cloudModels.length)void loadCloudModels();
  await Promise.allSettled([refreshHistory(),refreshRuntime()]);ensureRules();
  if(platform.os==='android'){await syncNativeOperation();await syncNativeRecording();await importQuickRecordings();}
  // A crash during a desktop recording leaves a repaired WAV in temp; archive it instead of losing it.
  if(!preview&&['macos','linux','windows'].includes(platform.os)){try{const recovered=await call<AudioSelection[]>('recover_audio_recordings');for(const selection of recovered)await importSelection(selection);if(recovered.length)notice(`${tr('Восстановлено записей после сбоя:')} ${recovered.length}`);}catch(error){notice(errorText(error),true);}}
  await refreshLoose();if(loose.length&&!status)notice(`${tr('Не обработано записей:')} ${loose.length}. ${tr('Они в «Архиве».')}`,true);
  if(!preview)void checkUpdates();
}
void initialize();
void initializeTransfers({refresh:refreshAfterTransfer,busy:()=>busy,setBusy:value=>{busy=value;render();},notice,confirm:(heading,message)=>confirmAction(heading,message,tr("Отключить"))});
subscribe<null>('hearfolio-request-restart',()=>{void restartAfterUpdate(false);}).catch(error=>notice(errorText(error),true));
setInterval(()=>{if(!preview&&Date.now()-lastUpdateCheck>=4*60*60*1000)void checkUpdates();},4*60*60*1000);
document.addEventListener('visibilitychange',()=>{if(!preview&&document.visibilityState==='visible'&&Date.now()-lastUpdateCheck>=4*60*60*1000)void checkUpdates();});
subscribe<Progress>('task-progress',payload=>{if(!busy||!progress||payload.kind!==progress.kind)return;progress={...payload,stage:errorText(payload.stage),elapsed:Math.max(payload.elapsed,Math.floor((Date.now()-operationStarted)/1000))};feedProgress();updateProgress();}).catch(error=>notice(errorText(error),true));
function paintPartial(){
  const result=document.querySelector('#result');if(result){const follow=result.scrollHeight-result.scrollTop-result.clientHeight<90;result.innerHTML=outputHtml();if(follow)result.scrollTop=result.scrollHeight;}
  const meta=document.querySelector('#text-meta');if(meta)meta.innerHTML=recordMetaHtml();
}
subscribe<PartialTextEvent>('partial-text',payload=>{
  if(!busy||progress?.kind!=='transcribe'||!payload.text)return;
  if(payload.recordId&&payload.recordId!==active?.id)return;
  if(nativeOperation?.id&&payload.operationId&&payload.operationId!==nativeOperation.id)return;
  if(!receivedPartial)void call<Settings>('get_settings').then(saved=>{settings=saved;}).catch(()=>{});receivedPartial=true;text=payload.text;resultSaved=false;
  if(!revealing||revealRecord!==active?.id)startReveal(0);feedReveal();
  const meta=document.querySelector('#text-meta');if(meta)meta.innerHTML=recordMetaHtml();
}).catch(error=>notice(errorText(error),true));
// Desktop runs the same cancellable operation guard; poll its id so the progress card can offer Cancel.
setInterval(()=>{
  if(preview||platform.os==='android'||!busy||!progress||!['transcribe','postprocess','import'].includes(progress.kind)||nativeOperation?.state==='running')return;
  void call<OperationSnapshot>('operation_activity_snapshot').then(snapshot=>{if(busy&&snapshot.state==='running'&&snapshot.kind===progress?.kind){nativeOperation=snapshot;updateProgress();}}).catch(()=>{});
},1000);
if(preview){
  document.addEventListener('dragover',event=>{event.preventDefault();if(!busy)document.querySelector('.main-surface')?.classList.add('drop-hover');});
  document.addEventListener('dragleave',event=>{if(!event.relatedTarget)document.querySelector('.main-surface')?.classList.remove('drop-hover');});
  document.addEventListener('drop',event=>{event.preventDefault();document.querySelector('.main-surface')?.classList.remove('drop-hover');const file=event.dataTransfer?.files[0];if(file&&!busy)void choose(registerPreviewFile(file));});
}
document.addEventListener('keydown',event=>{
  if(document.querySelector('dialog[open]'))return;
  if(event.key==='Escape'&&catalogOpen){closeCatalog();return;}
  if(!(event.metaKey||event.ctrlKey))return;
  const key=event.key.toLowerCase();
  if(key==='o'){event.preventDefault();if(!busy)choose();}
  if(key==='n'){event.preventDefault();if(!busy)void newRecord();}
  if(key==='k'){event.preventDefault();changeView('history');}
  if(key==='enter'&&view==='work'){event.preventDefault();run();}
  if(key==='b'){event.preventDefault();setSidebar(!sidebarCollapsed);}
});
window.addEventListener('beforeunload',event=>{if(settingsDraft||settingsSaving||hasTransferDraft()||hasRuleDraft()||pendingRecording||busy||(text&&!resultSaved))event.preventDefault();});

function updateCatalogViewport(){const viewport=window.visualViewport;document.documentElement.style.setProperty('--catalog-height',`${viewport?.height||window.innerHeight}px`);document.documentElement.style.setProperty('--catalog-top',`${viewport?.offsetTop||0}px`);}
updateCatalogViewport();window.visualViewport?.addEventListener('resize',updateCatalogViewport);window.visualViewport?.addEventListener('scroll',updateCatalogViewport);window.addEventListener('resize',updateCatalogViewport);

subscribe<NativeCapture>('recording-native-state',p=>{if(!['recording','paused','stopping'].includes(recordingPhase)||recordingControlPending)return;if(p.path&&nativeRecordingPath&&p.path!==nativeRecordingPath)return;if(!applyNativeCapture(p))return;if(p.state==='finished'){clearRecordingTimer();recordingPhase='stopping';render();void syncNativeRecording(true);return;}const next=p.state==='paused'?'paused':p.state==='recording'?'recording':p.state==='stopping'?'stopping':null;if(next&&next!==recordingPhase){if(next==='stopping')clearRecordingTimer();recordingPhase=next;render();}}).catch(()=>{});
type NativeRecordingResult={entry?:Entry;selection?:AudioSelection;error?:string;path?:string;stateRevision?:number};
let lastNativeRecordingResult='';
async function finishNativeRecording(p:NativeRecordingResult){const key=JSON.stringify(p),receipt=p.entry?`recording:${p.entry.id}`:'';if(key===lastNativeRecordingResult||(receipt&&operationHandled(receipt))){if(recordingPhase!=='idle'){resetRecorderUi();render();}return;}lastNativeRecordingResult=key;if(receipt)markOperationHandled(receipt);resetRecorderUi();pendingRecording=p.selection||null;if(p.entry){resetPlayback();active=p.entry;restoreConfiguration(p.entry);text='';outputTab='transcript';resultSaved=false;previousResult=null;view='work';await refreshHistory();}render();if(p.error)notice(errorText(p.error),true);else notice(tr('Запись сохранена'));}
subscribe<NativeRecordingResult>('recording-native-finished',p=>{if(nativeRecordingOrder.acceptResult(p))void finishNativeRecording(p);}).catch(()=>{});
let quickFinishing=false;
async function finishQuickRecording(){if(quickFinishing)return;quickFinishing=true;try{const entries=await call<Entry[]>('import_quick_recordings');externalRecording=false;recordingPhase='idle';busy=false;clearRecordingTimer();if(entries.length){const entry=entries[entries.length-1];if(!active||resultSaved||!text){resetPlayback();active=entry;restoreConfiguration(entry);text='';outputTab='transcript';resultSaved=false;previousResult=null;}await refreshHistory();notice(tr('Запись сохранена'));}render();}catch(e){if(!String(e).includes('operationBusy')){externalRecording=false;recordingPhase='idle';busy=false;render();notice(errorText(e),true);}}finally{quickFinishing=false;}}
async function syncNativeRecording(afterControl=false){
  if((document.hidden&&!afterControl)||platform.os!=='android'||((recordingPhase==='cancelling'||recordingControlPending)&&!afterControl))return;
  return recordingSnapshotPoll.read(()=>readNativeRecordingSnapshot(),afterControl);
}
async function readNativeRecordingSnapshot(){
  const generation=nativeCaptureGeneration;
  try{
    const p=await call<NativeCapture&{quick?:boolean;error?:string;interrupted?:boolean;result?:NativeRecordingResult}>('recording_activity_snapshot');
    if(generation!==nativeCaptureGeneration||!applyNativeCapture(p))return;
    const live=p.state==='recording'||p.state==='paused';
    restoreNativeRecordingFlag(live&&!p.quick);
    if(p.state==='starting'){
      externalRecording=Boolean(p.quick);busy=true;view='work';
      if(recordingPhase!=='starting'){recordingPhase='starting';render();}
    }else if(p.state==='stopping'){
      externalRecording=Boolean(p.quick);busy=true;view='work';
      if(recordingPhase!=='stopping'){clearRecordingTimer();recordingPhase='stopping';render();}
    }else if(live){
      if(busy&&recordingPhase==='idle')return;
      externalRecording=Boolean(p.quick);busy=true;view='work';
      const changed=recordingPhase!==p.state;recordingPhase=p.state as 'recording'|'paused';
      const timer=document.querySelector('#recording-duration');if(timer)timer.textContent=recordingDuration();
      if(changed)render();
    }else if(busy&&recordingPhase==='idle')return;
    else if(externalRecording){
      if(p.state==='finished'){recordingPhase='stopping';await finishQuickRecording();if(p.error)notice(errorText(p.error),true);}
      else{externalRecording=false;recordingPhase='idle';busy=false;render();if(p.error)notice(errorText(p.error),true);}
    }else if(p.result&&nativeRecordingOrder.acceptResult(p.result)){await finishNativeRecording(p.result);}
    else if(p.state==='error'&&recordingPhase!=='idle')await finishNativeRecording({error:p.error||'errors.recordingEmpty'});
    else if(['idle','cancelled'].includes(p.state)&&recordingPhase!=='idle'){resetRecorderUi();render();}
  }catch{/* A failed read cannot change the microphone's confirmed state. */}
}
document.addEventListener('visibilitychange',()=>{if(!document.hidden)void syncNativeRecording();});
let lastNativePoll=0;setInterval(()=>{const now=performance.now();if(now-lastNativePoll>=(recordingPhase==='idle'?1200:300)){lastNativePoll=now;void syncNativeRecording();}},150);


let importingQuick=false,quickImportFailed=false;
async function importQuickRecordings(retry=false){if(retry)quickImportFailed=false;if(quickImportFailed||importingQuick||busy||platform.os!=='android')return;importingQuick=true;try{const entries=await call<Entry[]>('import_quick_recordings');if(entries.length){await refreshHistory();notice(currentLanguage()==='ru'?`Добавлено записей: ${entries.length}`:`Recordings added: ${entries.length}`);}}catch(e){if(!String(e).includes('operationBusy')){quickImportFailed=true;notice(`${errorText(e)} ${tr('Аудио сохранено в разделе «Архив».')}`,true);void refreshLoose();}}finally{importingQuick=false;}}
document.addEventListener('visibilitychange',()=>{if(!document.hidden)void importQuickRecordings(true);});

setInterval(()=>{if(!document.hidden&&platform.os==='android')void importQuickRecordings();},5000);

async function consumeNativeOperation(snapshot:OperationSnapshot){
  if(!snapshot.id||!snapshot.kind||snapshot.state==='idle')return;
  if(snapshot.kind!=='transcribe'&&snapshot.kind!=='postprocess'&&snapshot.kind!=='import')return;
  const pending=observePendingOperation(snapshot);
  if(pending.terminal)return; // The original UI action now receives this result.
  if(operationHandled(snapshot.id)||recoveringOperation)return;
  if(snapshot.state==='running'){
    if(!pending.matched&&busy&&!nativeOperation)return;
    const changed=nativeOperation?.id!==snapshot.id;
    nativeOperation=snapshot;
    if(!pending.matched&&changed){
      recoveringOperation=true;
      try{
        if(snapshot.recordId){const detail=await call<{entry:Entry;text:string}>('get_history',{id:snapshot.recordId});resetPlayback();active=detail.entry;restoreConfiguration(active);text=detail.text;resultSaved=Boolean(active.outputPath&&detail.text);}
        if(snapshot.partial){text=snapshot.partial;resultSaved=false;receivedPartial=true;}
        if(snapshot.kind==='transcribe')startReveal(text.length);
        beginOperation(snapshot.kind,errorText(snapshot.progress?.stage||`progress.${snapshot.kind}`));
        view='work';outputTab=snapshot.kind==='postprocess'?'results':'transcript';
        restorePostprocessActivity(snapshot.recordId||null,snapshot.kind==='postprocess');
        render();
      }catch(error){nativeOperation=null;notice(errorText(error),true);}finally{recoveringOperation=false;}
    }
    if(snapshot.progress){progress={...snapshot.progress,kind:snapshot.kind,stage:errorText(snapshot.progress.stage)};operationStarted=Date.now()-progress.elapsed*1000;feedProgress();}
    if(snapshot.kind==='transcribe'&&snapshot.partial&&active?.id===snapshot.recordId&&snapshot.partial!==text){text=snapshot.partial;resultSaved=false;receivedPartial=true;if(!revealing)startReveal(0);feedReveal();}
    updateProgress();return;
  }
  // A newly opened WebView has no pending invoke. Read the already committed
  // archive rather than starting another (possibly paid) request.
  if(busy&&!nativeOperation)return;
  recoveringOperation=true;
  try{
    const imported=snapshot.kind==='import'&&snapshot.state==='finished'?snapshot.result as Entry:null;
    const id=imported?.id||snapshot.recordId;
    stopReveal();
    if(id){const detail=await call<{entry:Entry;text:string}>('get_history',{id});resetPlayback();active=detail.entry;restoreConfiguration(active);text=detail.text;resultSaved=Boolean(active.outputPath&&detail.text);previousResult=null;view='work';outputTab=snapshot.kind==='postprocess'?'results':'transcript';}
    if(imported)pendingRecording=null;
    restorePostprocessActivity(null,false);endOperation();
    await refreshHistory();markOperationHandled(snapshot.id);
    if(snapshot.resumable&&id)resume={id,...snapshot.resumable} as CloudResume&{id:string};
    notice(snapshot.state==='finished'?tr(snapshot.kind==='import'?'Запись сохранена':snapshot.kind==='postprocess'?'Результат сохранён':'Готово'):`${errorText(snapshot.error||'errors.operationInterrupted')}${snapshot.resumable?` ${tr('Готово частей:')} ${snapshot.resumable.done}/${snapshot.resumable.total}. ${tr('Расшифровку можно продолжить.')}`:''}`,snapshot.state!=='finished');
  }catch(error){notice(errorText(error),true);}finally{recoveringOperation=false;}
}
async function syncNativeOperation(){
  if(preview||operationPolling||document.hidden||platform.os!=='android')return;
  operationPolling=true;
  try{await consumeNativeOperation(await call<OperationSnapshot>('operation_activity_snapshot'));}catch{/* The persisted snapshot can be read on the next foreground tick. */}finally{operationPolling=false;}
}
subscribe<OperationSnapshot>('operation-native-finished',snapshot=>{if(platform.os==='android')void consumeNativeOperation(snapshot);}).catch(()=>{});
document.addEventListener('visibilitychange',()=>{if(!document.hidden)void syncNativeOperation();});
setInterval(()=>{void syncNativeOperation();},1000);
