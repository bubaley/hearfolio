import {recordNameParts, renamedRecordTitle} from './record-name';
import {normalizeAudioCatalog, readAudioCatalog, saveAudioCatalog} from './cloud-catalog';
import {RecorderEnvelope,NativeRecordingOrder,NativeRecorderClock,RecordingSnapshotPoll} from './recorder-meter';
import {pauseAudioRecording, resumeAudioRecording, restoreNativeRecordingFlag, observePendingOperation, operationHandled, markOperationHandled} from './api';
import type {OperationSnapshot} from './operation-recovery';
import {transferSettingsHtml, devicesHtml, transferBarHtml, shareRecordingToDevice, bindTransfers, initializeTransfers, hasTransferDraft, discardTransferDraft, transferActivityLabel} from './transfers';
import './styles.css';
import './transfers.css';
import {renderSettings, initialSettingsDraft, mergeSettingsDraft, hasSettingsChanges, isSettingsSection, settingsLabel, type SettingsSection, type SettingsDraft} from './settings-ui';
import {recordingInputLevel, call, pickAudio, startAudioRecording, stopAudioRecording, cancelAudioRecording, discardAudioRecording, exportText, subscribe, subscribeAudioDrop, registerPreviewFile, audioSource, shareAudio, shareText, saveAudio, prepareNativePlayback, nativePlaybackState, playNativePlayback, pauseNativePlayback, seekNativePlayback, releaseNativePlayback, type NativePlayerState, openExternal, openAndroidBatterySettings, pickStorageParent, loadRuntimePlatform, previewCapabilities, appVersion, checkDesktopUpdate, latestAndroidRelease, restartApp, isNewerVersion, preview, type AudioSelection, type Entry, type Progress, type Settings, type CloudModel, type RecognitionConfig, type RuntimeCapabilities, type AndroidRelease, type AppUpdate} from './api';
import {icon, escapeHtml as esc} from './icons';
import {tr, locale, countLabel, errorText, setLanguage, currentLanguage, type LanguagePreference} from './i18n';

import {setupPostprocess, rulesHtml, postprocessHtml, resultsHtml, textPickerHtml, textPickerOpen, bindPostprocess, ensureRules, hasRuleDraft, mayLeaveRules, restorePostprocessActivity} from './postprocess';

type View = 'work' | 'history' | 'models' | 'settings' | 'rules' | 'devices';
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
let settings:Settings={language:'system',systemLanguage:currentLanguage(),openrouterUrl:'https://openrouter.ai/api/v1',hasToken:false,storageParent:'',storagePath:'',lastConfiguration:{provider:'local',model:'whisper-base',mode:'local'}};
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
let playerId='', playerLoading=false, playerError='', playerState:NativePlayerState={duration:0,currentTime:0,playing:false}, playerPoll:ReturnType<typeof setTimeout>|null=null, playerGeneration=0;
const microphone=()=>'<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" aria-hidden="true"><rect x="9" y="2" width="6" height="13" rx="3"/><path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8"/></svg>';
let nativeRecordingSeconds:number|null=null,nativeRecordingMillis:number|null=null,nativeRecordingPath='',externalRecording=false,recordingControlPending=false,nativeCaptureGeneration=0;
const recorderEnvelope=new RecorderEnvelope();
const nativeRecordingOrder=new NativeRecordingOrder();
const nativeRecorderClock=new NativeRecorderClock();
const recordingSnapshotPoll=new RecordingSnapshotPoll();
type NativeCapture={state:string;stateRevision?:number;path?:string;elapsedSeconds?:number;elapsedMillis?:number;level?:number;envelope?:number[];envelopeSequence?:number;envelopeIntervalMs?:number};
function recordingMilliseconds(){return nativeRecordingMillis!==null?nativeRecorderClock.milliseconds(performance.now()):(nativeRecordingSeconds!==null?nativeRecordingSeconds*1000:recordingStarted?Math.max(0,(recordingPausedAt||Date.now())-recordingStarted-recordingPausedMillis):0);}
function recorderRulerHtml(){const seconds=recordingMilliseconds()/1000,span=recorderEnvelope.size*recorderEnvelope.interval/1000;return Array.from({length:5},(_,index)=>{const time=seconds+(index-2)*span/2;return `<span>${time>=0?formatTime(time):''}</span>`;}).join('');}
function recordingMeterHtml(){return `<div class="recorder-timeline" role="img" aria-label="${tr('Уровень микрофона')}"><div class="recorder-ruler" aria-hidden="true">${recorderRulerHtml()}</div><div class="recorder-wave"><div class="recorder-bars">${recorderEnvelope.levels().map(level=>`<i style="--level:${level}"></i>`).join('')}</div><div class="recorder-playhead" aria-hidden="true"></div></div></div>`;}
function paintRecorderMeter(){const meter=document.querySelector('.recorder-bars');if(meter)meter.querySelectorAll<HTMLElement>('i').forEach((bar,index)=>bar.style.setProperty('--level',String(recorderEnvelope.levels()[index])));const ruler=document.querySelector('.recorder-ruler');if(ruler)ruler.innerHTML=recorderRulerHtml();const timer=document.querySelector('#recording-duration');if(timer)timer.textContent=recordingDuration();}
function applyNativeCapture(p:NativeCapture){
  const sameSession=!p.path||p.path===nativeRecordingPath;
  const old=Boolean(['recording','paused'].includes(p.state)&&sameSession&&p.elapsedMillis!==undefined&&nativeRecordingMillis!==null&&p.elapsedMillis<nativeRecordingMillis);
  if(old||!nativeRecordingOrder.accept(p))return false;
  if(p.path&&p.path!==nativeRecordingPath){nativeRecordingPath=p.path;nativeRecordingMillis=null;nativeRecordingSeconds=null;nativeRecorderClock.reset();recorderEnvelope.reset();}
  if(p.elapsedMillis!==undefined){nativeRecordingMillis=p.elapsedMillis;nativeRecorderClock.synchronize(p.elapsedMillis,p.state,performance.now());}
  if(p.elapsedSeconds!==undefined)nativeRecordingSeconds=p.elapsedSeconds;
  if(p.envelope&&p.envelopeSequence!==undefined&&p.path)recorderEnvelope.synchronize(p.envelope,p.envelopeSequence,p.path);
  paintRecorderMeter();return true;
}
function updateRecorderMeter(){
  if(document.hidden||!['recording','paused'].includes(recordingPhase))return;
  if(platform.os==='android'&&!preview){paintRecorderMeter();return;}
  if(recordingPhase==='recording'&&recorderEnvelope.sample(recordingInputLevel(),performance.now(),true))paintRecorderMeter();
}
setInterval(updateRecorderMeter,125);
let recordingPausedAt=0,recordingPausedMillis=0;
const recordingDuration=()=>{const millis=recordingMilliseconds();return `${String(Math.floor(millis/60000)).padStart(2,'0')}:${String(Math.floor(millis/1000)%60).padStart(2,'0')}.${String(Math.floor(millis/10)%100).padStart(2,'0')}`;};
document.documentElement.dataset.theme=theme;
const title:Record<View,string>={work:"Расшифровка",history:"Записи",models:"Модели",settings:"Настройки",devices:"Устройства",rules:"Правила обработки"};
const modelName=(id:string|null)=>{const normalized=id?.replace(/^openrouter\//,'');return models.find(model=>model.id===normalized)?.name||cloudModels.find(model=>model.id===normalized)?.name||normalized||tr("Модель не выбрана");};
const formatTime=(seconds:number)=>`${Math.floor(seconds/60)}:${String(Math.floor(seconds)%60).padStart(2,'0')}`;
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
  busy=true;status='';operationStarted=Date.now();progress={kind,stage,current:0,total:0,elapsed:0};
  if (elapsedTimer) clearInterval(elapsedTimer);
  elapsedTimer=setInterval(()=>{if(progress){progress.elapsed=Math.max(progress.elapsed,Math.floor((Date.now()-operationStarted)/1000));updateProgress();}},1000);
}
function endOperation() {busy=false;progress=null;nativeOperation=null;operationCancelling=false;if(elapsedTimer)clearInterval(elapsedTimer);elapsedTimer=null;}
function progressHtml():string {
  if (!busy||!progress) return '';
  const p=progress,percent=p.total?Math.min(100,Math.floor(p.current/p.total*100)):null;
  const remaining=p.elapsed>0&&p.current>0&&p.total>p.current?Math.ceil((p.total-p.current)/(p.current/p.elapsed)):0;
  return `<div class="task-progress" role="status"><div class="progress-label"><span class="spinner"></span><strong>${esc(p.stage)}</strong><span>${percent===null?'':`${percent}%`}</span></div><div class="track ${percent===null?'indeterminate':''}"><i style="width:${percent??25}%"></i></div><div class="progress-detail"><span>${(p.kind==='download'||p.kind==='update')&&p.total?`${(p.current/1048576).toFixed(1)} / ${(p.total/1048576).toFixed(1)}${tr(" МБ · ")}`:''}${tr("Прошло ")}${formatTime(p.elapsed)}</span><span>${remaining?`${tr("Осталось около ")}${formatTime(remaining)}`:tr("Оцениваем время")}</span></div>${nativeOperation?.state==='running'?`<button type="button" class="text-button" id="cancel-operation" ${operationCancelling?'disabled':''}>${tr(operationCancelling?'Завершаем отмену…':'Отмена')}</button>`:''}</div>`;
}
function updateProgress() {const element=document.querySelector('#progress');if(element)element.innerHTML=progressHtml();bindOperationCancel();}
function bindOperationCancel(){document.querySelector('#cancel-operation')?.addEventListener('click',()=>{void cancelNativeOperation();});}
async function cancelNativeOperation(){if(!nativeOperation?.id||operationCancelling)return;operationCancelling=true;updateProgress();try{await call('cancel_background_operation',{id:nativeOperation.id});}catch(error){operationCancelling=false;notice(errorText(error),true);updateProgress();}}
function groupLabel(timestamp:number) {
  const day=new Date(timestamp);day.setHours(0,0,0,0);const today=new Date();today.setHours(0,0,0,0);
  const age=Math.round((today.getTime()-day.getTime())/86400000);
  return age===0?tr("Сегодня"):age===1?tr("Вчера"):age<7?tr("Предыдущие 7 дней"):age<30?tr("Предыдущие 30 дней"):tr("Ранее");
}
function recordActionsHtml(entry:Entry):string {
  return `<div class="record-actions"><button class="icon-button" data-rename="${esc(entry.id)}${tr("\" aria-label=\"Переименовать ")}${esc(entry.name)}${tr("\" title=\"Переименовать запись\" ")}${disabled()}>${icon('edit')}</button><button class="icon-button" data-delete="${esc(entry.id)}${tr("\" aria-label=\"Удалить ")}${esc(entry.name)}${tr("\" title=\"Удалить запись\" ")}${disabled()}>${icon('trash')}</button></div>`;
}
function historyItems(entries:Entry[],compact=false):string {
  let group='';
  return entries.map(entry=>{
    const label=groupLabel(entry.createdAt),heading=!compact&&label!==group?`<div class="history-group">${label}</div>`:'';group=label;
    return `${heading}<div class="${compact?'recent-item':'history-item'} ${active?.id===entry.id?'selected':''}"><button class="record-link" data-history="${esc(entry.id)}" ${disabled()} title="${esc(entry.name)}"><span class="record-title">${!compact?icon('wave'):''}${esc(entry.name)}</span>${!compact?`<span class="record-detail">${dateLabel(entry.createdAt)}${entry.durationSeconds?` · ${formatTime(entry.durationSeconds)}`:''}${entry.sizeBytes?` · ${(entry.sizeBytes/1048576).toFixed(1)}${tr(" МБ")}`:''}</span>`:''}</button>${!compact?`<span class="record-model">${esc(modelName(entry.model||entry.configuration?.model||null))}</span><span class="record-state">${entry.outputPath?icon('check')+tr("Готово"):tr("Аудио")}</span>`:''}${recordActionsHtml(entry)}</div>`;
  }).join('');
}
function desktopModelsNav():string {return platform.localRecognition?`<button class="nav-item ${view==='models'?'active':''}" data-view="models">${icon('models')}<span>${tr('Модели')}</span>${runtimeLoaded?`<small>${models.filter(model=>model.installed).length}</small>`:''}</button>`:'';}
function sidebarHtml():string {
  if(platform.mobile)return '';
  const recent=history.slice(0,window.innerHeight<700?4:6);
  return `${tr("<aside class=\"sidebar\" aria-label=\"Навигация\"><div class=\"sidebar-chrome\" data-tauri-drag-region><button class=\"icon-button\" id=\"collapse\" title=\"Свернуть боковую панель\" aria-label=\"Свернуть боковую панель\" aria-expanded=\"true\">")}${icon('panel')}${tr("</button></div><div class=\"sidebar-body\"><div class=\"brand-row\"><button class=\"brand\" data-view=\"work\" title=\"Hearfolio — расшифровка аудио\">")}${icon('wave')}Hearfolio</button></div><div class="primary-nav"><button class="nav-item" id="new" ${disabled()}>${icon('plus')}${tr("<span>Новая запись</span><kbd>⌘ N</kbd></button><button class=\"nav-item ")}${view==='history'?'active':''}" data-view="history">${icon('search')}${tr("<span>Найти запись</span><kbd>⌘ K</kbd></button></div><div class=\"recent-records\"><div class=\"recent-heading\"><span>Недавние записи</span>")}${history.length?`${tr("<button class=\"text-button\" data-view=\"history\">Все ")}${history.length}</button>`:''}</div>${loading?tr("<div class=\"sidebar-empty\">Загружаем записи…</div>"):historyError?tr("<button class=\"text-button\" id=\"retry-history\">Повторить загрузку</button>"):recent.length?historyItems(recent,true):tr("<div class=\"sidebar-empty\">Здесь появятся ваши записи</div>")}</div><nav class="bottom-nav" aria-label="${tr('Приложение')}">${desktopModelsNav()}<button class="nav-item ${view==='devices'?'active':''}" data-view="devices">${icon('devices')}<span>${tr('Устройства')}</span></button><button class="nav-item ${view==='settings'?'active':''}" data-view="settings">${icon('settings')}${tr("<span>Настройки</span></button></nav><div class=\"sidebar-footer\"><span>")}${icon('headphones')}${tr("Личный аудиоархив</span><button class=\"icon-button\" data-theme title=\"")}${theme==='dark'?tr("Светлая тема"):tr("Тёмная тема")}" aria-label="${theme==='dark'?tr("Светлая тема"):tr("Тёмная тема")}">${icon(theme==='dark'?'sun':'moon')}</button></div></div></aside>`;
}
function captureHtml():string {
  if(recordingPhase!=='idle'){
    const live=['recording','paused'].includes(recordingPhase),paused=recordingPhase==='paused';
    const label=tr(recordingPhase==='starting'?'Подключаем микрофон…':recordingPhase==='stopping'?'Сохраняем аудио…':recordingPhase==='cancelling'?'Отменяем запись…':paused?'Запись на паузе':'Идёт запись');
    return `<section class="capture-panel recording-panel ${paused?'recorder-paused':''}" aria-label="${tr('Диктофон')}"><div class="recorder-heading"><span class="recorder-dot" aria-hidden="true"></span><span>${label}</span></div><div class="recording-duration" id="recording-duration" aria-label="${tr('Длительность записи')}">${recordingDuration()}</div>${recordingMeterHtml()}<div class="recorder-meter-caption"><span>${tr('Записанный звук')}</span><span>${tr('Сейчас')}</span></div><div class="recorder-actions"><button type="button" class="secondary recorder-pause" id="pause-recording" ${!live||recordingControlPending?'disabled':''}>${icon(paused?'play':'pause')}<span>${tr(paused?'Продолжить':'Пауза')}</span></button><button type="button" class="primary capture-stop" id="stop-recording" ${!live||recordingControlPending?'disabled':''}><span class="stop-symbol"></span><span>${tr('Сохранить')}</span></button></div><button type="button" class="text-button capture-cancel" id="cancel-recording" ${!live||recordingControlPending?'disabled':''}>${tr('Отменить запись')}</button></section>`;
  }
  if(pendingRecording)return `<div class="capture-panel"><div class="empty-symbol">${microphone()}</div><h1>${tr('Аудио записано')}</h1><p>${tr('Добавьте запись в архив, чтобы перейти к распознаванию.')}</p><button class="primary" id="retry-recording" ${disabled()}>${tr('Добавить в архив')}</button><button class="text-button capture-cancel" id="discard-recording" ${disabled()}>${tr('Удалить эту запись')}</button></div>`;
  return `<div class="capture-panel"><div class="empty-symbol">${icon('wave')}</div><h1>${tr('Начните с аудио')}</h1><p>${tr('Запишите голосовую заметку или выберите готовый файл.')}</p>${platform.audioRecording?`<button class="primary capture-start" id="start-recording" ${disabled()}>${microphone()}${tr('Записать аудио')}</button>`:''}<button class="secondary capture-file" data-choose ${disabled()}>${icon('plus')}${tr('Выбрать аудиофайл')}</button><small>${audioFormats}</small></div>`;
}
function outputHtml():string {
  if(recordingPhase!=='idle')return captureHtml();
  if(!active&&(platform.mobile||recordingPhase!=='idle'||pendingRecording))return captureHtml();
  if(active&&outputTab==='results')return resultsHtml();
  if (text) return `<article class="transcript">${esc(text)}</article>`;
  if (active) return `<div class="record-empty"><h2>${busy&&progress?.kind==='transcribe'?tr("Распознаём запись…"):tr("Аудио готово")}</h2><p>${busy&&progress?.kind==='transcribe'?tr("Текст будет появляться здесь по мере обработки."):tr("Выберите модель и нажмите «Распознать».")}</p></div>`;
  return `<div class="welcome" id="drop-area"><div class="empty-symbol">${icon('wave')}</div><h1>${tr('Превратите запись в текст')}</h1><p>${tr('Встреча, интервью или голосовая заметка —')}<br>${tr(platform.audioRecording?'выберите аудиофайл или запишите голос.':'добавьте аудиофайл, чтобы начать.')}</p><div class="welcome-actions"><button class="secondary choose-file" data-choose ${disabled()}>${icon('plus')}${tr('Выбрать аудиофайл')}<kbd>⌘ O</kbd></button>${platform.audioRecording?`<button class="secondary choose-file" id="start-recording" ${disabled()}>${microphone()}${tr('Записать аудио')}</button>`:''}</div><small>${tr('Или перетащите файл сюда')} · ${audioFormats}</small></div>`;
}
function resultMeta():string {return busy&&progress?.kind==='transcribe'?tr("Распознавание…"):text?`${countLabel(words(),'word')} · ${resultSaved?tr("Сохранено в архиве"):tr("Неполный текст · не сохранён")}`:tr("Аудиофайл");}
function providerControlsHtml():string {
  if(platform.mobile)return `<div class="mobile-recognition"><div class="recognition-service"><span>${tr('Сервис')}</span><strong>${icon('cloud')}OpenRouter</strong></div>${cloudControlsHtml()}</div>`;
  if(!platform.localRecognition)return `<span class="mobile-provider">${icon('cloud')}OpenRouter</span><span class="control-divider"></span>${cloudControlsHtml()}`;
  return `${tr("<label class=\"control-select provider-select\"><span class=\"sr-only\">Способ распознавания</span>")}${icon(configuration.provider==='local'?'headphones':'cloud')}<select id="provider" ${disabled()}><option value="local" ${configuration.provider==='local'?'selected':''}${tr(">На устройстве</option><option value=\"openrouter\" ")}${configuration.provider==='openrouter'?'selected':''}>OpenRouter</option></select></label><span class="control-divider"></span>${configuration.provider==='local'?`${tr("<label class=\"control-select model-select\"><span class=\"sr-only\">Локальная модель</span><select id=\"local-model\" ")}${disabled()}>${models.map(model=>`<option value="${model.id}" ${configuration.model===model.id?'selected':''}>${model.name}${model.installed?'':tr(" · скачать")}</option>`).join('')}</select></label>`:cloudControlsHtml()}`;
}
function cloudControlsHtml():string {
  const model=cloudModels.find(item=>item.id===configuration.model),label=model?.name||configuration.model||tr('Выбрать модель');
  const button=`<button type="button" class="cloud-model-control" id="cloud-picker-toggle" aria-haspopup="dialog" aria-expanded="${catalogOpen}" ${disabled()}><span>${esc(label)}</span>${icon('chevron')}</button>`;
  const mode=model?(configuration.mode==='transcription'?tr('Аудиотранскрибация'):tr('Потоковый текст')):tr('Выбирается автоматически');
  if(platform.mobile)return `<div class="recognition-model"><span class="recognition-label">${tr('Модель')}</span>${button}</div><div class="recognition-mode-row"><span>${tr('Режим')}</span><span>${mode}</span></div>`;
  return `<div class="cloud-picker">${button}${model?`<small class="recognition-mode">${mode}</small>`:''}${catalogOpen?catalogHtml():''}</div>`;
}
function catalogHtml():string {
  const matches=cloudModels.filter(model=>`${model.name} ${model.id}`.toLocaleLowerCase().includes(catalogSearch.toLocaleLowerCase()));
  return `${tr("<section class=\"model-catalog\" role=\"dialog\" aria-label=\"Модели OpenRouter\"><div class=\"catalog-heading\"><strong>Модели OpenRouter</strong><button type=\"button\" class=\"icon-button\" id=\"catalog-close\" aria-label=\"Закрыть\">")}${icon('close')}</button></div><label class="search-field">${icon('search')}${tr("<input type=\"search\" id=\"catalog-search\" placeholder=\"Найти модель\" value=\"")}${esc(catalogSearch)}${tr("\" aria-label=\"Найти модель\"></label><div id=\"catalog-results\">")}${catalogResults(matches)}</div><button type="button" class="text-button" id="catalog-refresh" ${cloudLoading?'disabled':''}${tr(">Обновить каталог</button></section>")}`;
}
function catalogResults(matches:CloudModel[]):string {
  if(cloudLoading&&!cloudModels.length)return tr("<div class=\"catalog-empty\"><span class=\"spinner\"></span> Загружаем модели…</div>");
  const warning=catalogError?`<div class="catalog-empty" role="alert">${esc(catalogError)}</div>`:'';
  if(catalogError&&!cloudModels.length)return warning;
  if(!matches.length)return `<div class="catalog-empty">${cloudModels.length?tr("Модели не найдены"):tr("Нет моделей с поддержкой аудио")}</div>`;
  return `${warning}<div class="catalog-list">${matches.map(model=>`<button type="button" class="catalog-option ${configuration.model===model.id?'selected':''}" data-cloud-model="${esc(model.id)}"><span><strong>${esc(model.name)}</strong><small>${esc(model.id)}</small></span>${configuration.model===model.id?icon('check'):''}</button>`).join('')}</div>`;
}
function runButtonHtml(issue:string):string {
  const label=resultSaved?tr("Распознать ещё раз"):tr("Распознать");
  return `<button class="run-button ${active&&!busy?'with-label':''}" id="run" ${busy||!active||issue?'disabled':''} title="${esc(issue||(!active?tr("Добавьте аудиофайл"):label+' · ⌘ Enter'))}" aria-label="${label}">${active&&!busy?`<span>${label}</span>`:''}${busy&&progress?.kind==='transcribe'?'<span class="spinner"></span>':icon('arrow')}</button>`;
}
function composerHtml(issue:string):string {
  if(recordingPhase!=='idle')return '';
  if(!active&&(platform.mobile||recordingPhase!=='idle'||pendingRecording))return progress?`<div class="composer-area"><div id="progress">${progressHtml()}</div></div>`:'';
  const needsDownload=configuration.provider==='local'&&runtime.ffmpeg&&!models.find(model=>model.id===localChoice)?.installed;
  const needsCatalog=configuration.provider==='openrouter'&&(platform.nativeAudio||runtime.ffmpeg)&&settings.hasToken;
  const action=needsDownload?`data-download="${localChoice}"`:needsCatalog?(catalogError?'id="retry-catalog"':'id="open-catalog"'):configuration.provider==='openrouter'&&(platform.nativeAudio||runtime.ffmpeg)?'data-view="settings" data-settings-target="recognition"':'data-view="models"';
  return `<div class="composer-area"><div class="composer ${active?'has-record':''}">
    <div id="progress">${progressHtml()}</div>
    ${!active?`<div class="composer-top"><button class="attachment" data-choose ${disabled()}>${icon('plus')}${tr("<span>Добавить аудиофайл</span><kbd>⌘ O</kbd></button>")}${runButtonHtml(issue)}</div>`:''}
    <div class="composer-controls">${providerControlsHtml()}${active?runButtonHtml(issue):''}</div>
    ${issue?`<div class="composer-requirement"><span>${esc(issue)}</span>${runtimeLoaded?`<button class="text-button" ${action} ${busy||cloudLoading?'disabled':''}>${needsDownload?tr("Скачать модель"):needsCatalog?tr(catalogError?'Обновить каталог':'Выбрать модель'):tr("Настроить")}${icon('chevron')}</button>`:''}</div>`:''}
  </div></div>`;
}
function workHtml():string {
  const issue=readiness(),source=active&&!platform.mobile?audioSource(active):'';
  return `<section class="workspace">
    ${recordingPhase==='idle'?transferBarHtml(active):''}
    ${active&&recordingPhase==='idle'?`<div class="record-toolbar"><div class="record-info">${icon('wave')}<div><div class="record-name-row"><h1 title="${esc(active.name)}">${esc(active.name)}</h1><button class="icon-button" data-rename="${esc(active.id)}" aria-label="${tr('Переименовать запись')}" ${disabled()}>${icon('edit')}</button></div><span id="text-meta">${dateLabel(active.createdAt)} · ${resultMeta()}</span></div></div><button type="button" class="secondary record-share-button" id="record-share" aria-haspopup="dialog" aria-expanded="false" aria-controls="record-actions-dialog" aria-label="${tr('Поделиться записью')}">${icon('share')}<span>${tr('Поделиться')}</span></button></div><div class="audio-bar ${platform.mobile?'native-audio-bar':''}">${platform.mobile?nativePlayerHtml():source?`<audio id="audio" controls controlslist="nodownload noplaybackrate" preload="metadata" src="${esc(source)}" aria-label="${tr('Прослушать ')}${esc(active.name)}"></audio>`:''}<p id="playback-error" class="playback-error" role="alert"></p></div>${text||active.results?.length?`<div class="output-tabs"><div class="output-tab-buttons" role="tablist" aria-label="${tr('Текст записи')}"><button role="tab" id="transcript-tab" data-output-tab="transcript" aria-selected="${outputTab==='transcript'}">${tr('Расшифровка')}</button><button role="tab" id="results-tab" data-output-tab="results" aria-selected="${outputTab==='results'}">${tr('Результаты')}${active.results?.length?` <small>${active.results.length}</small>`:''}</button></div>${text&&resultSaved&&outputTab==='transcript'?`<button type="button" id="process-shortcut" class="process-shortcut" ${busy?'disabled':''}>${tr('Обработать текст')}</button>`:''}</div>`:''}`:''}
    <div class="transcript-scroll" id="result" ${active&&(text||active.results?.length)?`role="tabpanel" aria-labelledby="${outputTab==='transcript'?'transcript-tab':'results-tab'}"`:''}>${outputHtml()}${active&&text&&resultSaved&&outputTab==='transcript'?postprocessHtml():''}</div>
    ${previousResult?.saved&&!busy&&!resultSaved?tr("<div class=\"restore-result\"><span>Предыдущая расшифровка сохранена в архиве.</span><button class=\"text-button\" id=\"restore\">Вернуть предыдущий текст</button></div>"):''}
    ${active&&outputTab==='results'?`<div class="results-process-footer"><button class="secondary" data-output-tab="transcript" ${busy?'disabled':''}>${tr('Обработать текст')}</button></div>`:''}
    ${outputTab==='results'&&active?`<div class="composer-area"><div id="progress">${progressHtml()}</div></div>`:composerHtml(issue)}
  </section>`;
}
function nativePlayerHtml():string {
  const duration=playerState.duration||active?.durationSeconds||0;
  return `<div class="native-player" aria-label="${tr('Прослушать ')}${esc(active?.name||'')}"><button type="button" id="native-play" class="icon-button" aria-label="${tr(playerState.playing?'Пауза':'Воспроизвести')}" ${playerLoading||playerError?'disabled':''}>${playerLoading?'<span class="spinner"></span>':icon(playerState.playing?'pause':'play')}</button><div class="player-track"><input type="range" id="native-seek" min="0" max="${Math.max(duration,1)}" value="${playerState.currentTime}" step="0.1" aria-label="${tr('Позиция воспроизведения')}" ${playerLoading||playerError||!duration?'disabled':''}><div><span id="native-position">${formatTime(playerState.currentTime)}</span><span id="native-duration">${duration?formatTime(duration):'—'}</span></div></div></div>${playerError?`<div class="player-error" role="alert"><span>${esc(playerError)}</span><button type="button" class="text-button" id="retry-playback">${tr('Повторить')}</button></div>`:''}`;
}
function historyResults():string {
  const entries=history.filter(entry=>entry.name.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  if (loading) return tr("<div class=\"empty-state\"><span class=\"spinner\"></span><p>Загружаем записи…</p></div>");
  if (historyError) return `<div class="empty-state">${icon('alert')}${tr("<h2>Не удалось загрузить записи</h2><p>")}${esc(historyError)}${tr("</p><button class=\"secondary\" id=\"retry-history\">Повторить</button></div>")}`;
  if (!entries.length) return `<div class="empty-state">${icon(search?'search':'history')}<h2>${search?tr("Записи не найдены"):tr("Пока нет записей")}</h2><p>${search?tr("Попробуйте другое название."):tr("Добавьте аудиофайл — он появится здесь вместе с расшифровкой.")}</p>${search?'':tr("<button class=\"secondary\" data-choose>Добавить аудиофайл</button>")}</div>`;
  return historyItems(entries);
}
function historyHtml():string {
  return `${tr("<div class=\"page-heading\"><div><h1>Ваши записи</h1><p>Аудио и расшифровки, сохранённые на этом устройстве.</p></div><span class=\"count\">")}${history.length}</span></div><div class="history-tools"><label class="search-field">${icon('search')}${tr("<input id=\"history-search\" type=\"search\" placeholder=\"Найти по названию\" value=\"")}${esc(search)}${tr("\" aria-label=\"Найти запись\"></label><button class=\"icon-button danger\" id=\"clear\" title=\"Очистить архив\" aria-label=\"Очистить архив\" ")}${busy||!history.length?'disabled':''}>${icon('trash')}</button></div><div class="history-list" id="history-results">${historyResults()}</div>`;
}
function modelsHtml():string {
  const installationCommand=platform.os==='linux'?'sudo apt install ffmpeg':'brew install ffmpeg whisper.cpp';
  const installationHelp=platform.os==='linux'?tr('На Debian и Ubuntu установите FFmpeg этой командой. Whisper установите по инструкции проекта whisper.cpp.'):tr('Команда использует Homebrew. Модель скачивается отдельно ниже.');
  return `${tr("<div class=\"page-heading\"><div><h1>Модели на устройстве</h1><p>Скачайте один раз. Дальше записи можно распознавать без интернета.</p></div></div><div class=\"runtime-panel\"><div><h2>Готовность к распознаванию</h2><button class=\"icon-button\" id=\"retry-runtime\" title=\"Проверить снова\" aria-label=\"Проверить готовность снова\">")}${icon('history')}</button></div><div class="runtime-checks">${[{name:'FFmpeg',ready:runtime.ffmpeg},{name:'Whisper',ready:runtime.whisper},{name:'NeMo',ready:runtime.nemo}].map(item=>`<span class="runtime-check ${item.ready?'ready':''}">${icon(item.ready?'check':'alert')}${item.name}<small>${runtimeLoaded?(item.ready?tr("Готов"):tr("Не установлен")):tr("Проверяем")}</small></span>`).join('')}</div>${runtimeLoaded&&(!runtime.ffmpeg||!runtime.whisper)?`${tr("<p>Для Whisper установите инструменты в Терминале, затем проверьте готовность снова.</p><div class=\"install-command\"><code>")}${installationCommand}${tr("</code><button class=\"icon-button\" id=\"copy-install\" title=\"Копировать команду\" aria-label=\"Копировать команду установки\">")}${icon('copy')}</button></div><p class="small-copy">${installationHelp}</p>${platform.os==='linux'?`<a href="https://github.com/ggml-org/whisper.cpp" target="_blank" rel="noreferrer">${tr('Инструкция whisper.cpp')}${icon('chevron')}</a>`:''}`:''}</div><div class="model-list">${models.map(model=>`<article class="model-card"><div class="model-copy"><h2>${model.name}${localChoice===model.id?tr("<span class=\"badge\">Выбрана</span>"):''}</h2><p>${tr(model.detail)}</p><small>${tr(model.size)}${model.installed?tr(" · Скачана"):''}</small></div>${model.installed?`<button class="secondary" data-select="${model.id}" ${busy||localChoice===model.id?'disabled':''}>${localChoice===model.id?icon('check')+tr("Выбрана"):tr("Использовать")}</button>`:`<button class="secondary" data-download="${model.id}" ${disabled()}>${icon('download')}${tr("Скачать</button>")}`}</article>`).join('')}${tr("</div><div class=\"help-note\"><h2>Для Nemotron нужен NeMo</h2><p>Установите NeMo-Speech.cpp по инструкции проекта. Движок добавит команду <code>nemo-speech</code>; затем проверьте готовность снова.</p><a href=\"https://github.com/NVIDIA/NeMo-Speech.cpp\" target=\"_blank\" rel=\"noreferrer\">Инструкция NeMo-Speech.cpp ")}${icon('chevron')}</a></div>`;
}
function settingsHtml():string {
  return renderSettings({settings,draft:settingsDraft,section:settingsSection||(!platform.mobile&&window.innerWidth>760?'general':null),mobile:platform.mobile,android:platform.os==='android',theme,busy,saving:settingsSaving,error:settingsError,customStorage:platform.customStorage,transferMvp:Boolean(platform.transferMvp),version:applicationVersion,devicesHtml:transferSettingsHtml(),updatesHtml:updatesHtml()});
}
function mobileNavHtml():string {
  return `<nav class="mobile-nav" aria-label="${tr('Навигация')}">${(['work','history',...(platform.localRecognition?['models']:[]),'devices','settings'] as View[]).map(target=>`<button type="button" data-view="${target}" class="${view===target||(target==='settings'&&view==='rules')?'active':''}" ${recordingPhase!=='idle'||(target==='work'&&busy)?'disabled':''}>${icon(target==='work'?'wave':target==='history'?'history':target)}<span>${tr(title[target])}</span></button>`).join('')}</nav>`;
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
function render() {
  preservePlayback();
  document.documentElement.dataset.platform=platform.os;
  app.innerHTML=`<div ${(platform.mobile&&catalogOpen)||textPickerOpen()?'inert':''} class="shell ${sidebarCollapsed?'sidebar-collapsed':''} ${platform.mobile?'mobile-platform':''}">${sidebarHtml()}<main><header class="main-header" data-tauri-drag-region><div class="header-title" data-tauri-drag-region>${!platform.mobile?`${tr("<button class=\"icon-button\" id=\"expand\" title=\"Показать боковую панель\" aria-label=\"Показать боковую панель\" aria-expanded=\"false\">")}${icon('panel')}</button>`:''}<span data-tauri-drag-region>${esc(view==='work'?(tr(active?'Запись':recordingPhase!=='idle'?'Диктофон':'Новая расшифровка')):tr(title[view]))}</span></div><button type="button" class="icon-button mobile-new" id="mobile-new" ${recordingPhase!=='idle'?'hidden':''} aria-label="${tr('Новая запись')}" ${disabled()}>${icon('plus')}</button><div class="header-status"><div id="update-header">${updateHeaderHtml()}</div>${busy&&recordingPhase==='idle'?`<span class="header-activity"><span class="spinner"></span>${recordingPhase!=='idle'?'':progress?.kind==='download'?tr("Скачиваем модель"):progress?.kind==='import'?tr("Добавляем запись"):progress?.kind==='storage'?tr('Переносим архив'):progress?.kind==='update'?tr('Устанавливаем обновление'):progress?.kind==='postprocess'?tr('Обрабатываем текст…'):progress?.kind==='transcribe'?tr('Распознаём'):transferActivityLabel()||tr('Выполняем…')}</span>`:''}${preview?tr("<span class=\"preview-label\">Предпросмотр</span>"):''}</div></header><div class="main-surface ${view==='work'?'work-surface':view==='settings'?'settings-surface':''}">${view==='work'?workHtml():`<div class="page-scroll ${view==='devices'?'devices-scroll':''}"><div class="page-content ${view==='devices'?'devices-page':''}">${view==='history'?transferBarHtml(null)+historyHtml():view==='models'?modelsHtml():view==='devices'?devicesHtml():view==='rules'?rulesHtml():settingsHtml()}${busy?`<div id="progress">${progressHtml()}</div>`:''}</div></div>`}<div id="status" class="notice ${isError?'error':''}" role="status" aria-live="polite"></div></div>${mobileNavHtml()}</main></div>${platform.mobile&&catalogOpen?`<div class="catalog-backdrop" id="catalog-backdrop">${catalogHtml()}</div>`:''}${textPickerHtml()}`;
  bind();updateNotice();restorePlayback();if(view==='work'&&active&&platform.mobile&&recordingPhase==='idle')void ensureNativePlayer();else stopPlayerPolling();
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
async function changeView(target:View, settingsTarget?:SettingsSection) {
  if(recordingPhase!=='idle'||busy)return;
  if(view==='rules'&&target!=='rules'&&!await mayLeaveRules())return;
  if(target==='models'&&!platform.localRecognition)return;
  if((view==='settings'||view==='devices')&&target!==view&&!await mayLeaveSettings())return;
  if(target==='settings') {
    if(view==='settings'&&settingsTarget&&settingsTarget!==settingsSection&&!await mayLeaveSettings())return;
    if(view!=='settings')settingsSection=settingsTarget||((platform.mobile||window.innerWidth<=760)?null:'general');
    else if(settingsTarget)settingsSection=settingsTarget;
  }
  view=target;status='';if(window.innerWidth<=760)sidebarCollapsed=true;render();if(target==='rules')ensureRules();if(target==='history'&&!platform.mobile&&window.innerWidth>760)document.querySelector<HTMLInputElement>('#history-search')?.focus();
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
  document.querySelector('#mobile-new')?.addEventListener('click',newRecord);
  document.querySelector('#run')?.addEventListener('click',run);
  document.querySelectorAll('button[data-theme]').forEach(element=>element.addEventListener('click',()=>{theme=theme==='dark'?'light':'dark';document.documentElement.dataset.theme=theme;localStorage.setItem('hearing-theme',theme);render();}));
  document.querySelector('#collapse')?.addEventListener('click',()=>setSidebar(true));document.querySelector('#expand')?.addEventListener('click',()=>setSidebar(false));
  document.querySelector<HTMLSelectElement>('#provider')?.addEventListener('change',event=>{const provider=(event.target as HTMLSelectElement).value as RecognitionConfig['provider'];chooseConfiguration(provider==='local'?{provider,model:localChoice,mode:'local'}:{provider,model:cloudChoice,mode:cloudModels.find(model=>model.id===cloudChoice)?.preferredMode||'streaming'});if(provider==='openrouter'&&!cloudModels.length)loadCloudModels();});
  document.querySelector<HTMLSelectElement>('#local-model')?.addEventListener('change',event=>{chooseConfiguration({provider:'local',model:(event.target as HTMLSelectElement).value,mode:'local'});});
  document.querySelectorAll<HTMLElement>('[data-select]').forEach(element=>element.onclick=()=>chooseConfiguration({provider:'local',model:element.dataset.select!,mode:'local'}));
  bindCatalog();bindPostprocess();bindNativePlayer();
  document.querySelector('#process-shortcut')?.addEventListener('click',()=>{const controls=document.querySelector<HTMLElement>('.postprocess-controls');if(controls){controls.scrollIntoView({behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'auto':'smooth',block:'start'});controls.focus({preventScroll:true});}});
  document.querySelectorAll<HTMLElement>('[data-output-tab]').forEach(element=>element.onclick=()=>{outputTab=element.dataset.outputTab as typeof outputTab;render();});
  document.querySelector('#record-share')?.addEventListener('click',openRecordActions);
  document.querySelectorAll<HTMLElement>('[data-download]').forEach(element=>element.onclick=()=>download(element.dataset.download!));
  bindRecordActions();
  document.querySelector<HTMLInputElement>('#history-search')?.addEventListener('input',event=>{search=(event.target as HTMLInputElement).value;document.querySelector('#history-results')!.innerHTML=historyResults();bindRecordActions();document.querySelectorAll('[data-choose]').forEach(element=>element.addEventListener('click',()=>choose()));});
  document.querySelector('#clear')?.addEventListener('click',clearHistory);
  document.querySelector('#retry-history')?.addEventListener('click',()=>refreshHistory());
  document.querySelector('#retry-runtime')?.addEventListener('click',()=>refreshRuntime());
  document.querySelector('#copy-install')?.addEventListener('click',()=>copyText(platform.os==='linux'?'sudo apt install ffmpeg':'brew install ffmpeg whisper.cpp'));
  document.querySelector('#restore')?.addEventListener('click',()=>{if(previousResult){text=previousResult.text;resultSaved=previousResult.saved;previousResult=null;render();}});
  document.querySelectorAll<HTMLAnchorElement>('a[href^="https://"]').forEach(anchor=>anchor.onclick=event=>{event.preventDefault();openExternal(anchor.href).catch(error=>notice(errorText(error),true));});
  bindSettings();bindUpdates();bindTransfers(active);
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
  const saved=await persist(draft.token.trim()||undefined,{language:draft.language,openrouterUrl:draft.url.trim()});
  settingsSaving=false;
  if(!saved){settingsError=settingsLabel('Настройки не сохранены. Ваши изменения остаются здесь; попробуйте ещё раз.','Settings were not saved. Your changes are still here; try again.');render();return false;}
  settingsDraft=null;if(connectionChanged){cloudModels=readAudioCatalog(localStorage,settings.openrouterUrl);catalogError='';void loadCloudModels();}render();notice(tr("Настройки применены"));return true;
}
async function persist(token?:string,snapshot:{language:LanguagePreference;openrouterUrl:string}={language:settings.language,openrouterUrl:settings.openrouterUrl}):Promise<boolean> {
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
  if(active&&(resultSaved||!text)) {
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
async function newRecord() {
  if(busy||!await mayLeaveSettings()||!await mayReplaceRecord())return;
  resetPlayback();settingsDraft=null;active=null;restoreConfiguration();text='';outputTab='transcript';resultSaved=false;previousResult=null;status='';view='work';render();
}
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
function recordingName(selected:AudioSelection):string {
  const date=new Date(recordingStarted||Date.now()),pad=(value:number)=>String(value).padStart(2,'0');
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
  if(active){notice(tr('Для другого файла создайте новую запись.'));return;}
  if(!await mayLeaveSettings()||!await mayReplaceRecord())return;
  pickingAudio=true;
  try{const selected=path?{path}:await pickAudio();if(selected&&!busy&&!active)await importSelection(selected);}
  catch(error){notice(`${tr('Не удалось добавить запись: ')}${errorText(error)}`,true);}
  finally{pickingAudio=false;}
}
async function loadHistory(id:string) {
  if(busy||!await mayLeaveSettings())return;
  if(active?.id===id){settingsDraft=null;view='work';render();return;}
  if(!await mayReplaceRecord())return;
  try {const detail=await call<{entry:Entry;text:string;warning?:string|null}>('get_history',{id});resetPlayback();active=detail.entry;restoreConfiguration(detail.entry);text=detail.text;outputTab='transcript';ensureRules();resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;settingsDraft=null;view='work';status=detail.warning?errorText(detail.warning):'';isError=Boolean(detail.warning);render();}
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
  if(!await confirmAction(tr("Удалить запись?"),`«${entry.name}${tr("» и её расшифровка будут удалены из архива. Исходный файл останется на месте.")}`,tr("Удалить запись")))return;
  try{await call('delete_history',{id});history=history.filter(item=>item.id!==id);if(active?.id===id){resetPlayback();active=null;text='';outputTab='transcript';resultSaved=false;previousResult=null;}render();}
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
  catch(error){status=`${tr("Не удалось скачать модель: ")}${errorText(error)}`;isError=true;}
  finally{endOperation();render();}
}
async function run() {
  if(!active||busy||readiness())return;
  const id=active.id,beforeAttempt={text,saved:resultSaved};
  if(text&&resultSaved)previousResult={text,saved:true};
  receivedPartial=false;outputTab='transcript';
  beginOperation('transcribe',tr("Подготавливаем запись"));render();
  try {
    await settingsSaveQueue;await configurationSaveQueue;
    text=await call<string>('transcribe',{id,configuration:{...configuration}});
    const detail=await call<{entry:Entry;text:string;warning?:string|null}>('get_history',{id});active=detail.entry;resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;
    settings=await call<Settings>('get_settings');
    history=sortHistory(await call<Entry[]>('list_history'));
  } catch(error) {
    if(!receivedPartial){text=beforeAttempt.text;resultSaved=beforeAttempt.saved;if(resultSaved)previousResult=null;}
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
  if(['macos','linux','windows'].includes(platform.os))subscribeAudioDrop(paths=>{if(paths.length){if(paths.length>1)notice(tr("Добавляем первый файл. Остальные можно добавить по одному."));choose(paths[0]);}}).catch(error=>notice(errorText(error),true));
  cloudModels=readAudioCatalog(localStorage,settings.openrouterUrl);
  restoreConfiguration(active);render();
  if(configuration.provider==='openrouter'&&cloudModels.length)void loadCloudModels();
  await Promise.allSettled([refreshHistory(),refreshRuntime()]);ensureRules();
  if(platform.os==='android'){await syncNativeOperation();await syncNativeRecording();await importQuickRecordings();}
  if(!preview)void checkUpdates();
}
void initialize();
void initializeTransfers({refresh:refreshAfterTransfer,busy:()=>busy,setBusy:value=>{busy=value;render();},notice,confirm:(heading,message)=>confirmAction(heading,message,tr("Отключить"))});
subscribe<null>('hearfolio-request-restart',()=>{void restartAfterUpdate(false);}).catch(error=>notice(errorText(error),true));
setInterval(()=>{if(!preview&&Date.now()-lastUpdateCheck>=4*60*60*1000)void checkUpdates();},4*60*60*1000);
document.addEventListener('visibilitychange',()=>{if(!preview&&document.visibilityState==='visible'&&Date.now()-lastUpdateCheck>=4*60*60*1000)void checkUpdates();});
subscribe<Progress>('task-progress',payload=>{if(!busy||!progress||payload.kind!==progress.kind)return;progress={...payload,stage:errorText(payload.stage),elapsed:Math.max(payload.elapsed,Math.floor((Date.now()-operationStarted)/1000))};updateProgress();}).catch(error=>notice(errorText(error),true));
subscribe<{text:string}>('partial-text',payload=>{
  if(!busy||progress?.kind!=='transcribe'||!payload.text)return;
  if(!receivedPartial)void call<Settings>('get_settings').then(saved=>{settings=saved;}).catch(()=>{});receivedPartial=true;text=payload.text;resultSaved=false;
  const result=document.querySelector('#result');if(result){const follow=result.scrollHeight-result.scrollTop-result.clientHeight<90;result.innerHTML=outputHtml();if(follow)result.scrollTop=result.scrollHeight;}

  const meta=document.querySelector('#text-meta');if(meta)meta.textContent=`${active?dateLabel(active.createdAt)+' · ':''}${resultMeta()}`;
}).catch(error=>notice(errorText(error),true));
if(preview&&!previewCapabilities.mobile){
  document.addEventListener('dragover',event=>{event.preventDefault();if(!busy)document.querySelector('.main-surface')?.classList.add('drop-hover');});
  document.addEventListener('dragleave',event=>{if(!event.relatedTarget)document.querySelector('.main-surface')?.classList.remove('drop-hover');});
  document.addEventListener('drop',event=>{event.preventDefault();document.querySelector('.main-surface')?.classList.remove('drop-hover');const file=event.dataTransfer?.files[0];if(file&&!busy){if(active)notice(tr("Для другого файла создайте новую запись."));else choose(registerPreviewFile(file));}});
}
document.addEventListener('keydown',event=>{
  if(document.querySelector('dialog[open]'))return;
  if(event.key==='Escape'&&catalogOpen){closeCatalog();return;}
  if(!(event.metaKey||event.ctrlKey))return;
  const key=event.key.toLowerCase();
  if(key==='o'){event.preventDefault();if(!busy)choose();}
  if(key==='n'){event.preventDefault();if(!busy)newRecord();}
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
async function importQuickRecordings(retry=false){if(retry)quickImportFailed=false;if(quickImportFailed||importingQuick||busy||platform.os!=='android')return;importingQuick=true;try{const entries=await call<Entry[]>('import_quick_recordings');if(entries.length){await refreshHistory();notice(currentLanguage()==='ru'?`Добавлено записей: ${entries.length}`:`Recordings added: ${entries.length}`);}}catch(e){if(!String(e).includes('operationBusy')){quickImportFailed=true;notice(errorText(e),true);}}finally{importingQuick=false;}}
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
        beginOperation(snapshot.kind,errorText(snapshot.progress?.stage||`progress.${snapshot.kind}`));
        view='work';outputTab=snapshot.kind==='postprocess'?'results':'transcript';
        restorePostprocessActivity(snapshot.recordId||null,snapshot.kind==='postprocess');
        render();
      }catch(error){nativeOperation=null;notice(errorText(error),true);}finally{recoveringOperation=false;}
    }
    if(snapshot.progress){progress={...snapshot.progress,kind:snapshot.kind,stage:errorText(snapshot.progress.stage)};operationStarted=Date.now()-progress.elapsed*1000;}
    updateProgress();return;
  }
  // A newly opened WebView has no pending invoke. Read the already committed
  // archive rather than starting another (possibly paid) request.
  if(busy&&!nativeOperation)return;
  recoveringOperation=true;
  try{
    const imported=snapshot.kind==='import'&&snapshot.state==='finished'?snapshot.result as Entry:null;
    const id=imported?.id||snapshot.recordId;
    if(id){const detail=await call<{entry:Entry;text:string}>('get_history',{id});resetPlayback();active=detail.entry;restoreConfiguration(active);text=detail.text;resultSaved=Boolean(active.outputPath&&detail.text);previousResult=null;view='work';outputTab=snapshot.kind==='postprocess'?'results':'transcript';}
    if(imported)pendingRecording=null;
    restorePostprocessActivity(null,false);endOperation();
    await refreshHistory();markOperationHandled(snapshot.id);
    notice(snapshot.state==='finished'?tr(snapshot.kind==='import'?'Запись сохранена':snapshot.kind==='postprocess'?'Результат сохранён':'Готово'):errorText(snapshot.error||'errors.operationInterrupted'),snapshot.state!=='finished');
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
