import './styles.css';
import {call, pickAudio, exportText, subscribe, subscribeAudioDrop, registerPreviewFile, audioSource, openExternal, preview, type Entry, type Progress, type Settings, type CloudModel} from './api';
import {icon, escapeHtml as esc} from './icons';

type View = 'work' | 'history' | 'models' | 'settings';
type SettingsDraft = {url:string;token:string;model:string;mode:Settings['openrouterMode']};
const models = [
  {id:'whisper-tiny',name:'Whisper Tiny',detail:'Для коротких заметок. Быстрее остальных, но чаще ошибается.',size:'75 МБ',installed:false},
  {id:'whisper-base',name:'Whisper Base',detail:'Для повседневных записей. Хороший выбор для начала.',size:'142 МБ',installed:false},
  {id:'whisper-small',name:'Whisper Small',detail:'Для встреч и сложной речи. Точнее, требует больше времени.',size:'466 МБ',installed:false},
  {id:'nemotron-3.5',name:'Nemotron 3.5',detail:'Многоязычное распознавание. Нужен отдельный движок NeMo.',size:'708 МБ',installed:false},
];
const app = document.querySelector<HTMLDivElement>('#app')!;
let view:View='work', history:Entry[]=[], active:Entry|null=null, text='', busy=false, loading=true;
let status='', isError=false, progress:Progress|null=null, resultSaved=false, search='', cloudLoading=false;
let settings:Settings={provider:'local',localModel:localStorage.getItem('hearing-model')||'whisper-base',openrouterUrl:'https://openrouter.ai/api/v1',hasToken:false,openrouterMode:'streaming',openrouterModel:'google/gemini-2.5-flash'};
let settingsDraft:SettingsDraft|null=null, cloudModels:CloudModel[]=[];
let runtime={ffmpeg:false,whisper:false,nemo:false};
let runtimeLoaded=false, historyError='', sidebarCollapsed=localStorage.getItem('hearfolio-sidebar')==='collapsed'||(!localStorage.getItem('hearfolio-sidebar')&&window.innerWidth<=760);
let theme=localStorage.getItem('hearing-theme')||(matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light');
let operationStarted=0, elapsedTimer:ReturnType<typeof setInterval>|null=null;
let settingsSaveQueue:Promise<unknown>=Promise.resolve(), previousResult:{text:string;saved:boolean}|null=null;
let receivedPartial=false, audioPosition=0, audioWasPlaying=false, pickingAudio=false;
let noticeTimer:ReturnType<typeof setTimeout>|null=null;
document.documentElement.dataset.theme=theme;
const title:Record<View,string>={work:'Расшифровка',history:'Записи',models:'Модели',settings:'Настройки'};
const modelName=(id:string|null)=>models.find(model=>model.id===id)?.name||id||'Модель не выбрана';
const formatTime=(seconds:number)=>`${Math.floor(seconds/60)}:${String(Math.floor(seconds)%60).padStart(2,'0')}`;
const dateLabel=(timestamp:number)=>new Date(timestamp).toLocaleDateString('ru-RU',{day:'numeric',month:'long'});
const audioFormats='MP3, M4A, WAV, AAC, FLAC, OGG, MP4';
const recordCount=(count:number)=>`${count} ${count%10===1&&count%100!==11?'запись':count%10>=2&&count%10<=4&&(count%100<12||count%100>14)?'записи':'записей'}`;
const disabled=()=>busy?'disabled':'';
const words=()=>text.trim()?text.trim().split(/\s+/).length:0;
function readiness():string {
  if (!runtimeLoaded) return 'Проверяем готовность…';
  if (!runtime.ffmpeg) return 'Установите FFmpeg для подготовки аудио.';
  if (settings.provider==='openrouter') return !settings.hasToken?'Добавьте API token OpenRouter.':!settings.openrouterModel?'Выберите модель OpenRouter.':'';
  const chosen=models.find(model=>model.id===settings.localModel);
  if (!chosen?.installed) return 'Скачайте выбранную модель.';
  if (settings.localModel==='nemotron-3.5'&&!runtime.nemo) return 'Установите движок NeMo для этой модели.';
  if (settings.localModel!=='nemotron-3.5'&&!runtime.whisper) return 'Установите Whisper для локального распознавания.';
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
  if (element) {element.className=`notice ${isError?'error':''}`;element.innerHTML=status?`<span>${isError?icon('alert'):icon('check')}${esc(status)}</span><button class="icon-button" data-dismiss aria-label="Закрыть сообщение">${icon('close')}</button>`:'';element.querySelector('[data-dismiss]')?.addEventListener('click',()=>{status='';updateNotice();});}
}
function beginOperation(kind:Progress['kind'],stage:string) {
  if(noticeTimer){clearTimeout(noticeTimer);noticeTimer=null;}
  busy=true;status='';operationStarted=Date.now();progress={kind,stage,current:0,total:0,elapsed:0};
  if (elapsedTimer) clearInterval(elapsedTimer);
  elapsedTimer=setInterval(()=>{if(progress){progress.elapsed=Math.max(progress.elapsed,Math.floor((Date.now()-operationStarted)/1000));updateProgress();}},1000);
}
function endOperation() {busy=false;progress=null;if(elapsedTimer)clearInterval(elapsedTimer);elapsedTimer=null;}
function progressHtml():string {
  if (!busy||!progress) return '';
  const p=progress,percent=p.total?Math.min(100,Math.floor(p.current/p.total*100)):null;
  const remaining=p.elapsed>0&&p.current>0&&p.total>p.current?Math.ceil((p.total-p.current)/(p.current/p.elapsed)):0;
  return `<div class="task-progress" role="status"><div class="progress-label"><span class="spinner"></span><strong>${esc(p.stage)}</strong><span>${percent===null?'':`${percent}%`}</span></div><div class="track ${percent===null?'indeterminate':''}"><i style="width:${percent??25}%"></i></div><div class="progress-detail"><span>${p.kind==='download'&&p.total?`${(p.current/1048576).toFixed(1)} / ${(p.total/1048576).toFixed(1)} МБ · `:''}Прошло ${formatTime(p.elapsed)}</span><span>${remaining?`Осталось около ${formatTime(remaining)}`:'Оцениваем время'}</span></div></div>`;
}
function updateProgress() {const element=document.querySelector('#progress');if(element)element.innerHTML=progressHtml();}
function groupLabel(timestamp:number) {
  const day=new Date(timestamp);day.setHours(0,0,0,0);const today=new Date();today.setHours(0,0,0,0);
  const age=Math.round((today.getTime()-day.getTime())/86400000);
  return age===0?'Сегодня':age===1?'Вчера':age<7?'Предыдущие 7 дней':age<30?'Предыдущие 30 дней':'Ранее';
}
function recordActionsHtml(entry:Entry):string {
  return `<div class="record-actions"><button class="icon-button" data-rename="${esc(entry.id)}" aria-label="Переименовать ${esc(entry.name)}" title="Переименовать запись" ${disabled()}>${icon('edit')}</button><button class="icon-button" data-delete="${esc(entry.id)}" aria-label="Удалить ${esc(entry.name)}" title="Удалить запись" ${disabled()}>${icon('trash')}</button></div>`;
}
function historyItems(entries:Entry[],compact=false):string {
  let group='';
  return entries.map(entry=>{
    const label=groupLabel(entry.createdAt),heading=!compact&&label!==group?`<div class="history-group">${label}</div>`:'';group=label;
    return `${heading}<div class="${compact?'recent-item':'history-item'} ${active?.id===entry.id?'selected':''}"><button class="record-link" data-history="${esc(entry.id)}" ${disabled()} title="${esc(entry.name)}"><span class="record-title">${!compact?icon('wave'):''}${esc(entry.name)}</span>${!compact?`<span class="record-detail">${dateLabel(entry.createdAt)}${entry.durationSeconds?` · ${formatTime(entry.durationSeconds)}`:''}${entry.sizeBytes?` · ${(entry.sizeBytes/1048576).toFixed(1)} МБ`:''}</span>`:''}</button>${!compact?`<span class="record-model">${esc(modelName(entry.model))}</span><span class="record-state">${entry.outputPath?icon('check')+'Готово':'Аудио'}</span>`:''}${recordActionsHtml(entry)}</div>`;
  }).join('');
}
function sidebarHtml():string {
  const recent=history.slice(0,window.innerHeight<700?4:6);
  return `<aside class="sidebar" aria-label="Навигация"><div class="sidebar-chrome" data-tauri-drag-region><button class="icon-button" id="collapse" title="Свернуть боковую панель" aria-label="Свернуть боковую панель" aria-expanded="true">${icon('panel')}</button></div><div class="sidebar-body"><div class="brand-row"><button class="brand" data-view="work" title="Hearfolio — расшифровка аудио">${icon('wave')}Hearfolio</button></div><div class="primary-nav"><button class="nav-item" id="new" ${disabled()}>${icon('plus')}<span>Новая запись</span><kbd>⌘ N</kbd></button><button class="nav-item ${view==='history'?'active':''}" data-view="history">${icon('search')}<span>Найти запись</span><kbd>⌘ K</kbd></button></div><div class="recent-records"><div class="recent-heading"><span>Недавние записи</span>${history.length?`<button class="text-button" data-view="history">Все ${history.length}</button>`:''}</div>${loading?'<div class="sidebar-empty">Загружаем записи…</div>':historyError?'<button class="text-button" id="retry-history">Повторить загрузку</button>':recent.length?historyItems(recent,true):'<div class="sidebar-empty">Здесь появятся ваши записи</div>'}</div><nav class="bottom-nav" aria-label="Приложение"><button class="nav-item ${view==='models'?'active':''}" data-view="models">${icon('models')}<span>Модели</span>${!runtimeLoaded?'':`<small>${models.filter(model=>model.installed).length}</small>`}</button><button class="nav-item ${view==='settings'?'active':''}" data-view="settings">${icon('settings')}<span>Настройки</span></button></nav><div class="sidebar-footer"><span>${icon('headphones')}Личный аудиоархив</span><button class="icon-button" data-theme title="${theme==='dark'?'Светлая тема':'Тёмная тема'}" aria-label="${theme==='dark'?'Светлая тема':'Тёмная тема'}">${icon(theme==='dark'?'sun':'moon')}</button></div></div></aside>`;
}
function outputHtml():string {
  if (text) return `<article class="transcript">${esc(text)}</article>`;
  if (active) return `<div class="record-empty"><div class="empty-symbol">${icon('wave')}</div><h2>${busy&&progress?.kind==='transcribe'?'Распознаём запись…':'Запись готова к распознаванию'}</h2><p>${busy&&progress?.kind==='transcribe'?'Текст будет появляться здесь по мере обработки.':'Выберите способ распознавания внизу и нажмите «Распознать».'}</p></div>`;
  return `<div class="welcome" id="drop-area"><div class="empty-symbol">${icon('wave')}</div><h1>Превратите запись в текст</h1><p>Встреча, интервью или голосовая заметка —<br>добавьте аудиофайл, чтобы начать.</p><button class="secondary choose-file" data-choose ${disabled()}>${icon('plus')}Выбрать аудиофайл<kbd>⌘ O</kbd></button><small>Или перетащите файл сюда · ${audioFormats}</small></div>`;
}
function resultMeta():string {return busy&&progress?.kind==='transcribe'?'Распознавание…':text?`${words()} слов · ${resultSaved?'Сохранено в архиве':'Неполный текст · не сохранён'}`:'Аудиофайл';}
function providerControlsHtml():string {
  return `<label class="control-select provider-select"><span class="sr-only">Способ распознавания</span>${icon(settings.provider==='local'?'headphones':'cloud')}<select id="provider" ${disabled()}><option value="local" ${settings.provider==='local'?'selected':''}>На устройстве</option><option value="openrouter" ${settings.provider==='openrouter'?'selected':''}>OpenRouter</option></select></label>
    <span class="control-divider"></span>
    ${settings.provider==='local'?`<label class="control-select model-select"><span class="sr-only">Локальная модель</span><select id="local-model" ${disabled()}>${models.map(model=>`<option value="${model.id}" ${settings.localModel===model.id?'selected':''}>${model.name}${model.installed?'':' · скачать'}</option>`).join('')}</select></label>`:`<button class="cloud-model-control" data-view="settings" title="Настроить OpenRouter"><span>${esc(settings.openrouterModel||'Выбрать модель')}</span>${icon('settings')}</button>`}`;
}
function runButtonHtml(issue:string):string {
  const label=resultSaved?'Распознать ещё раз':'Распознать';
  return `<button class="run-button ${active&&!busy?'with-label':''}" id="run" ${busy||!active||issue?'disabled':''} title="${esc(issue||(!active?'Добавьте аудиофайл':label+' · ⌘ Enter'))}" aria-label="${label}">${active&&!busy?`<span>${label}</span>`:''}${busy&&progress?.kind==='transcribe'?'<span class="spinner"></span>':icon('arrow')}</button>`;
}
function composerHtml(issue:string):string {
  const needsDownload=settings.provider==='local'&&runtime.ffmpeg&&!models.find(model=>model.id===settings.localModel)?.installed;
  const action=needsDownload?`data-download="${settings.localModel}"`:settings.provider==='openrouter'&&runtime.ffmpeg?'data-view="settings"':'data-view="models"';
  return `<div class="composer-area"><div class="composer ${active?'has-record':''}">
    <div id="progress">${progressHtml()}</div>
    ${!active?`<div class="composer-top"><button class="attachment" data-choose ${disabled()}>${icon('plus')}<span>Добавить аудиофайл</span><kbd>⌘ O</kbd></button>${runButtonHtml(issue)}</div>`:''}
    <div class="composer-controls">${providerControlsHtml()}${active?runButtonHtml(issue):''}</div>
    ${issue?`<div class="composer-requirement"><span>${esc(issue)}</span>${runtimeLoaded?`<button class="text-button" ${action} ${disabled()}>${needsDownload?'Скачать модель':'Настроить'}${icon('chevron')}</button>`:''}</div>`:''}
  </div></div>`;
}
function workHtml():string {
  const issue=readiness(),source=active?audioSource(active):'';
  return `<section class="workspace">
    ${active?`<div class="record-toolbar"><div class="record-info">${icon('wave')}<div><div class="record-name-row"><h1 title="${esc(active.name)}">${esc(active.name)}</h1><button class="icon-button" data-rename="${esc(active.id)}" aria-label="Переименовать запись" title="Переименовать запись" ${disabled()}>${icon('edit')}</button></div><span id="text-meta">${resultMeta()}</span></div></div><div class="output-actions"><button class="icon-button" id="copy" title="Копировать текст" aria-label="Копировать текст" ${!text?'disabled':''}>${icon('copy')}</button><button class="icon-button" id="export" title="Экспортировать .txt" aria-label="Экспортировать текст" ${!text?'disabled':''}>${icon('download')}</button></div></div>${source?`<div class="audio-bar"><audio id="audio" controls preload="metadata" src="${esc(source)}" aria-label="Прослушать ${esc(active.name)}"></audio><span>${dateLabel(active.createdAt)}${active.sizeBytes?` · ${(active.sizeBytes/1048576).toFixed(1)} МБ`:''}</span></div>`:''}`:''}
    <div class="transcript-scroll" id="result">${outputHtml()}</div>
    ${previousResult?.saved&&!busy&&!resultSaved?`<div class="restore-result"><span>Предыдущая расшифровка сохранена в архиве.</span><button class="text-button" id="restore">Вернуть предыдущий текст</button></div>`:''}
    ${composerHtml(issue)}
  </section>`;
}
function historyResults():string {
  const entries=history.filter(entry=>entry.name.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  if (loading) return '<div class="empty-state"><span class="spinner"></span><p>Загружаем записи…</p></div>';
  if (historyError) return `<div class="empty-state">${icon('alert')}<h2>Не удалось загрузить записи</h2><p>${esc(historyError)}</p><button class="secondary" id="retry-history">Повторить</button></div>`;
  if (!entries.length) return `<div class="empty-state">${icon(search?'search':'history')}<h2>${search?'Записи не найдены':'Пока нет записей'}</h2><p>${search?'Попробуйте другое название.':'Добавьте аудиофайл — он появится здесь вместе с расшифровкой.'}</p>${search?'':'<button class="secondary" data-choose>Добавить аудиофайл</button>'}</div>`;
  return historyItems(entries);
}
function historyHtml():string {
  return `<div class="page-heading"><div><h1>Ваши записи</h1><p>Аудио и расшифровки, сохранённые на этом устройстве.</p></div><span class="count">${history.length}</span></div><div class="history-tools"><label class="search-field">${icon('search')}<input id="history-search" type="search" placeholder="Найти по названию" value="${esc(search)}" aria-label="Найти запись"></label><button class="icon-button danger" id="clear" title="Очистить архив" aria-label="Очистить архив" ${busy||!history.length?'disabled':''}>${icon('trash')}</button></div><div class="history-list" id="history-results">${historyResults()}</div>`;
}
function modelsHtml():string {
  return `<div class="page-heading"><div><h1>Модели на устройстве</h1><p>Скачайте один раз. Дальше записи можно распознавать без интернета.</p></div></div><div class="runtime-panel"><div><h2>Готовность к распознаванию</h2><button class="icon-button" id="retry-runtime" title="Проверить снова" aria-label="Проверить готовность снова">${icon('history')}</button></div><div class="runtime-checks">${[{name:'FFmpeg',ready:runtime.ffmpeg},{name:'Whisper',ready:runtime.whisper},{name:'NeMo',ready:runtime.nemo}].map(item=>`<span class="runtime-check ${item.ready?'ready':''}">${icon(item.ready?'check':'alert')}${item.name}<small>${runtimeLoaded?(item.ready?'Готов':'Не установлен'):'Проверяем'}</small></span>`).join('')}</div>${runtimeLoaded&&(!runtime.ffmpeg||!runtime.whisper)?`<p>Для Whisper установите инструменты в Терминале, затем проверьте готовность снова.</p><div class="install-command"><code>brew install ffmpeg whisper.cpp</code><button class="icon-button" id="copy-install" title="Копировать команду" aria-label="Копировать команду установки">${icon('copy')}</button></div><p class="small-copy">Команда использует Homebrew. Модель скачивается отдельно ниже.</p>`:''}</div><div class="model-list">${models.map(model=>`<article class="model-card"><div class="model-copy"><h2>${model.name}${settings.localModel===model.id?'<span class="badge">Выбрана</span>':''}</h2><p>${model.detail}</p><small>${model.size}${model.installed?' · Скачана':''}</small></div>${model.installed?`<button class="secondary" data-select="${model.id}" ${busy||settings.localModel===model.id?'disabled':''}>${settings.localModel===model.id?icon('check')+'Выбрана':'Использовать'}</button>`:`<button class="secondary" data-download="${model.id}" ${disabled()}>${icon('download')}Скачать</button>`}</article>`).join('')}</div><div class="help-note"><h2>Для Nemotron нужен NeMo</h2><p>Установите NeMo-Speech.cpp по инструкции проекта. Движок добавит команду <code>nemo-speech</code>; затем проверьте готовность снова.</p><a href="https://github.com/NVIDIA/NeMo-Speech.cpp" target="_blank" rel="noreferrer">Инструкция NeMo-Speech.cpp ${icon('chevron')}</a></div>`;
}
function settingsHtml():string {
  const draft=settingsDraft||{url:settings.openrouterUrl,token:'',model:settings.openrouterModel,mode:settings.openrouterMode};
  return `<div class="page-heading"><div><h1>Настройки</h1><p>Распознавание через OpenRouter и внешний вид.</p></div></div><form id="settings-form" class="settings-panel"><section class="settings-section"><div class="section-heading"><h2>OpenRouter</h2><span class="badge ${settings.hasToken?'success':''}">${settings.hasToken?'Токен подключён':'Нужен API token'}</span></div><p>Аудио отправляется выбранному провайдеру. Стоимость и обработка данных зависят от модели.</p><label class="field"><span>API token</span><input name="token" type="password" autocomplete="off" value="${esc(draft.token)}" placeholder="${settings.hasToken?'Введите новый токен для замены':'sk-or-…'}"><small>${settings.hasToken?'Оставьте поле пустым, чтобы использовать сохранённый токен.':'Токен хранится в настройках приложения на этом устройстве.'}</small></label>${settings.hasToken?'<button type="button" id="remove-token" class="text-button danger">Удалить сохранённый токен</button>':''}<label class="field"><span>Как получать текст</span><select name="mode"><option value="streaming" ${draft.mode==='streaming'?'selected':''}>Потоковый текст</option><option value="transcription" ${draft.mode==='transcription'?'selected':''}>Аудиотранскрибация</option></select><small id="mode-help">${modeHelp(draft.mode)}</small></label><label class="field"><span>Модель</span><input name="model" autocomplete="off" required value="${esc(draft.model)}" list="cloud-models" placeholder="ID модели, например google/gemini-2.5-flash"><datalist id="cloud-models">${cloudModels.map(model=>`<option value="${esc(model.id)}">${esc(model.name)}</option>`).join('')}</datalist><small>В потоковом режиме выберите модель с поддержкой входного аудио. Для аудиотранскрибации нужен совместимый API и ID модели.</small><button type="button" id="load-cloud" class="text-button" ${cloudLoading||busy?'disabled':''}>${cloudLoading?'Загружаем модели…':'Применить подключение и загрузить модели'}${icon('chevron')}</button></label><details class="advanced-settings"><summary>Адрес API</summary><label class="field"><span>API URL</span><input name="url" type="url" required value="${esc(draft.url)}" placeholder="https://openrouter.ai/api/v1"><small>Измените адрес для другого совместимого сервиса.</small></label></details><div class="settings-actions"><button class="primary" type="submit" ${busy||!settingsDraft?'disabled':''}>Применить</button><span>${settingsDraft?'Есть неприменённые изменения':''}</span></div></section><section class="settings-section"><h2>Внешний вид</h2><div class="setting-row"><div><strong>Тема приложения</strong><p>${theme==='dark'?'Тёмная':'Светлая'}</p></div><button type="button" data-theme class="secondary">${icon(theme==='dark'?'sun':'moon')}${theme==='dark'?'Включить светлую':'Включить тёмную'}</button></div></section><section class="settings-section"><h2>Ваш архив</h2><p>Импортированные записи и готовые тексты сохраняются на этом устройстве.</p><div class="archive-path"><span>Аудиофайлы</span><code>~/.hearing/input</code></div><div class="archive-path"><span>Расшифровки</span><code>~/.hearing/output</code></div></section></form>`;
}
function modeHelp(mode:Settings['openrouterMode']) {return mode==='streaming'?'Текст появляется по мере обработки. Требуется модель, принимающая аудио в чате.':'Текст приходит после обработки через аудио API. Проверьте, что выбранный сервис поддерживает транскрибацию.';}
function preservePlayback() {
  const audio=document.querySelector<HTMLAudioElement>('#audio');
  if (audio) {audioPosition=audio.currentTime;audioWasPlaying=!audio.paused;}
}
function render() {
  preservePlayback();
  app.innerHTML=`<div class="shell ${sidebarCollapsed?'sidebar-collapsed':''}">${sidebarHtml()}<main><header class="main-header" data-tauri-drag-region><div class="header-title" data-tauri-drag-region>${true?`<button class="icon-button" id="expand" title="Показать боковую панель" aria-label="Показать боковую панель" aria-expanded="false">${icon('panel')}</button>`:''}<span data-tauri-drag-region>${esc(view==='work'?(active?.name||'Новая расшифровка'):title[view])}</span></div><div class="header-status">${busy?`<span class="header-activity"><span class="spinner"></span>${progress?.kind==='download'?'Скачиваем модель':progress?.kind==='import'?'Добавляем запись':'Распознаём'}</span>`:''}${preview?'<span class="preview-label">Предпросмотр</span>':''}</div></header><div class="main-surface ${view==='work'?'work-surface':''}">${view==='work'?workHtml():`<div class="page-scroll"><div class="page-content">${view==='history'?historyHtml():view==='models'?modelsHtml():settingsHtml()}${busy?`<div id="progress">${progressHtml()}</div>`:''}</div></div>`}<div id="status" class="notice ${isError?'error':''}" role="status" aria-live="polite"></div></div></main></div>`;
  bind();updateNotice();restorePlayback();
}
function restorePlayback() {
  const audio=document.querySelector<HTMLAudioElement>('#audio');
  if (!audio) return;
  audio.addEventListener('loadedmetadata',()=>{if(audioPosition&&audioPosition<audio.duration)audio.currentTime=audioPosition;if(audioWasPlaying)audio.play().catch(()=>{});});
  audio.addEventListener('error',()=>{const bar=document.querySelector('.audio-bar');if(bar)bar.innerHTML=`<span class="playback-error">Не удалось воспроизвести этот формат. Файл можно распознать.</span>`;});
}
function resetPlayback() {audioPosition=0;audioWasPlaying=false;const audio=document.querySelector<HTMLAudioElement>('#audio');if(audio){audio.pause();audio.currentTime=0;}}
async function mayReplaceRecord() {return !text||resultSaved||await confirmAction('Неполный текст не сохранён','Скопируйте или экспортируйте текст, чтобы сохранить его. При переходе к другой записи этот результат будет потерян.','Продолжить без текста',false);}
async function mayLeaveSettings() {return !settingsDraft||await confirmAction('Изменения не применены','Примените настройки перед уходом или продолжите без этих изменений.','Продолжить без изменений',false);}
async function changeView(target:View) {
  if (view==='settings'&&target!=='settings') {if(!await mayLeaveSettings())return;settingsDraft=null;}
  view=target;status='';render();if(target==='history')document.querySelector<HTMLInputElement>('#history-search')?.focus();
}
function bindRecordActions() {
  document.querySelectorAll<HTMLElement>('[data-history]').forEach(element=>element.onclick=()=>loadHistory(element.dataset.history!));
  document.querySelectorAll<HTMLElement>('[data-delete]').forEach(element=>element.onclick=()=>deleteEntry(element.dataset.delete!));
  document.querySelectorAll<HTMLElement>('[data-rename]').forEach(element=>element.onclick=()=>renameEntry(element.dataset.rename!));
}
function bind() {
  document.querySelectorAll<HTMLElement>('[data-view]').forEach(element=>element.onclick=()=>changeView(element.dataset.view as View));
  document.querySelectorAll('[data-choose]').forEach(element=>element.addEventListener('click',()=>choose()));
  document.querySelector('#new')?.addEventListener('click',newRecord);
  document.querySelector('#run')?.addEventListener('click',run);
  document.querySelectorAll('button[data-theme]').forEach(element=>element.addEventListener('click',()=>{theme=theme==='dark'?'light':'dark';document.documentElement.dataset.theme=theme;localStorage.setItem('hearing-theme',theme);render();}));
  document.querySelector('#collapse')?.addEventListener('click',()=>setSidebar(true));document.querySelector('#expand')?.addEventListener('click',()=>setSidebar(false));
  document.querySelector<HTMLSelectElement>('#provider')?.addEventListener('change',event=>{settings.provider=(event.target as HTMLSelectElement).value as Settings['provider'];persist();render();});
  document.querySelector<HTMLSelectElement>('#local-model')?.addEventListener('change',event=>{settings.localModel=(event.target as HTMLSelectElement).value;persist();render();});
  document.querySelectorAll<HTMLElement>('[data-select]').forEach(element=>element.onclick=()=>{settings.localModel=element.dataset.select!;settings.provider='local';persist();render();});
  document.querySelectorAll<HTMLElement>('[data-download]').forEach(element=>element.onclick=()=>download(element.dataset.download!));
  bindRecordActions();
  document.querySelector<HTMLInputElement>('#history-search')?.addEventListener('input',event=>{search=(event.target as HTMLInputElement).value;document.querySelector('#history-results')!.innerHTML=historyResults();bindRecordActions();document.querySelectorAll('[data-choose]').forEach(element=>element.addEventListener('click',()=>choose()));});
  document.querySelector('#clear')?.addEventListener('click',clearHistory);
  document.querySelector('#retry-history')?.addEventListener('click',()=>refreshHistory());
  document.querySelector('#retry-runtime')?.addEventListener('click',()=>refreshRuntime());
  document.querySelector('#copy-install')?.addEventListener('click',()=>copyText('brew install ffmpeg whisper.cpp'));
  document.querySelector('#copy')?.addEventListener('click',()=>copyText(text));
  document.querySelector('#export')?.addEventListener('click',()=>exportText((active?.name||'Расшифровка').replace(/\.(m4a|mp3|wav|mp4|aac|flac|ogg)$/i,'').replace(/[\/\\:\u0000-\u001f]/g,'-')+'.txt',text).catch(error=>notice(String(error),true)));
  document.querySelector('#restore')?.addEventListener('click',()=>{if(previousResult){text=previousResult.text;resultSaved=previousResult.saved;previousResult=null;render();}});
  document.querySelectorAll<HTMLAnchorElement>('a[href^="https://"]').forEach(anchor=>anchor.onclick=event=>{event.preventDefault();openExternal(anchor.href).catch(error=>notice(String(error),true));});
  bindSettings();
}
function setSidebar(collapsed:boolean) {sidebarCollapsed=collapsed;localStorage.setItem('hearfolio-sidebar',collapsed?'collapsed':'expanded');render();}
async function copyText(value:string) {try{await navigator.clipboard.writeText(value);notice('Скопировано');}catch(error){notice(`Не удалось скопировать текст: ${String(error)}`,true);}}
function readSettingsDraft(form:HTMLFormElement):SettingsDraft {const data=new FormData(form);return {url:String(data.get('url')).trim(),token:String(data.get('token')).trim(),model:String(data.get('model')).trim(),mode:String(data.get('mode')) as Settings['openrouterMode']};}
function bindSettings() {
  const form=document.querySelector<HTMLFormElement>('#settings-form');
  form?.addEventListener('input',()=>{settingsDraft=readSettingsDraft(form);const button=form.querySelector<HTMLButtonElement>('[type=submit]');if(button)button.disabled=busy;const hint=form.querySelector('.settings-actions span');if(hint)hint.textContent='Есть неприменённые изменения';const help=form.querySelector('#mode-help');if(help)help.textContent=modeHelp(settingsDraft.mode);});
  form?.addEventListener('submit',async event=>{event.preventDefault();await applySettings();});
  document.querySelector('#remove-token')?.addEventListener('click',async()=>{if(await confirmAction('Удалить API token?','Для распознавания через OpenRouter потребуется подключить токен снова.','Удалить токен')){if(await persist(''))render();}});
  document.querySelector('#load-cloud')?.addEventListener('click',loadCloudModels);
}
async function applySettings():Promise<boolean> {
  const form=document.querySelector<HTMLFormElement>('#settings-form');
  if (!form||!form.reportValidity()) return false;
  const draft=readSettingsDraft(form),next={...settings,openrouterUrl:draft.url,openrouterModel:draft.model,openrouterMode:draft.mode};
  if (!await persist(draft.token||undefined,next)) return false;
  settings=next;settingsDraft=null;render();return true;
}
async function loadCloudModels() {
  if(cloudLoading||busy)return;
  if(!await applySettings())return;
  cloudLoading=true;render();
  try{cloudModels=await call<CloudModel[]>('list_openrouter_models');cloudLoading=false;render();if(!cloudModels.length)notice('В каталоге нет моделей с поддержкой аудио. Укажите ID совместимой модели вручную.',true);}
  catch(error){cloudLoading=false;render();notice(`Не удалось загрузить модели: ${String(error)}`,true);}
}
async function persist(token?:string,snapshot:Settings={...settings}):Promise<boolean> {
  localStorage.setItem('hearing-model',snapshot.localModel);
  const operation=settingsSaveQueue.then(async()=>{try{const saved=await call<Settings>('save_settings',{settings:snapshot,...(token!==undefined?{token}:{})});snapshot.hasToken=saved.hasToken;settings.hasToken=saved.hasToken;return true;}catch(error){notice(`Не удалось применить настройки: ${String(error)}`,true);return false;}});
  settingsSaveQueue=operation;return operation;
}
function sortHistory(entries:Entry[]) {return entries.sort((left,right)=>right.createdAt-left.createdAt);}
async function refreshHistory() {try{history=sortHistory(await call<Entry[]>('list_history'));historyError='';}catch(error){historyError=String(error);}loading=false;render();}
async function refreshRuntime() {
  const results=await Promise.allSettled([call<typeof runtime>('runtime_status'),call<{id:string;installed:boolean}[]>('model_status')]);
  if(results[0].status==='fulfilled'){runtime=results[0].value;runtimeLoaded=true;}else notice(`Не удалось проверить инструменты: ${String(results[0].reason)}`,true);
  if(results[1].status==='fulfilled'){const statuses=results[1].value;models.forEach(model=>model.installed=statuses.find(item=>item.id===model.id)?.installed||false);}else notice(`Не удалось проверить модели: ${String(results[1].reason)}`,true);
  render();
}
async function newRecord() {
  if(busy||!await mayLeaveSettings()||!await mayReplaceRecord())return;
  resetPlayback();settingsDraft=null;active=null;text='';resultSaved=false;previousResult=null;status='';view='work';render();
}
async function choose(path?:string) {
  if(busy||pickingAudio)return;
  if(active){notice('Для другого файла создайте новую запись.');return;}
  if(!await mayLeaveSettings()||!await mayReplaceRecord())return;
  pickingAudio=true;let importStarted=false;
  try {
    const selected=path||await pickAudio();if(!selected)return;
    if(busy||active){if(active)notice('Для другого файла создайте новую запись.');return;}
    importStarted=true;beginOperation('import','Добавляем запись');settingsDraft=null;view='work';render();
    const entry=await call<Entry>('import_audio',{path:selected});resetPlayback();active=entry;text='';resultSaved=false;previousResult=null;
    history=sortHistory(await call<Entry[]>('list_history'));
  } catch(error) {notice(`Не удалось добавить запись: ${String(error)}`,true);}
  finally {pickingAudio=false;if(importStarted){endOperation();render();}}
}
async function loadHistory(id:string) {
  if(busy||!await mayLeaveSettings())return;
  if(active?.id===id){settingsDraft=null;view='work';render();return;}
  if(!await mayReplaceRecord())return;
  try {const detail=await call<{entry:Entry;text:string;warning?:string|null}>('get_history',{id});resetPlayback();active=detail.entry;text=detail.text;resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;settingsDraft=null;view='work';status=detail.warning||'';isError=Boolean(detail.warning);render();}
  catch(error) {notice(`Не удалось открыть запись: ${String(error)}`,true);}
}
async function renameEntry(id:string) {
  if(busy||document.querySelector('dialog[open]'))return;
  const entry=history.find(item=>item.id===id);if(!entry)return;
  const dialog=document.createElement('dialog');dialog.className='confirmation rename-dialog';
  dialog.setAttribute('aria-labelledby','rename-title');
  dialog.innerHTML=`<form><h2 id="rename-title">Переименовать запись</h2><label class="field"><span>Название</span><input name="record-name" value="${esc(entry.name)}" maxlength="200" required autocomplete="off" aria-describedby="rename-error"></label><p id="rename-error" class="rename-error" role="alert" hidden></p><div class="rename-actions"><button type="button" class="secondary" data-cancel>Отмена</button><button type="submit" class="primary">Сохранить</button></div></form>`;
  document.body.append(dialog);
  const input=dialog.querySelector<HTMLInputElement>('input')!,form=dialog.querySelector<HTMLFormElement>('form')!;
  const errorText=dialog.querySelector<HTMLElement>('#rename-error')!,saveButton=dialog.querySelector<HTMLButtonElement>('[type=submit]')!,cancelButton=dialog.querySelector<HTMLButtonElement>('[data-cancel]')!;
  let saving=false;
  const close=()=>{dialog.close();dialog.remove();};
  input.addEventListener('input',()=>{input.setCustomValidity('');errorText.hidden=true;});
  cancelButton.onclick=()=>{if(!saving)close();};
  dialog.addEventListener('cancel',event=>{event.preventDefault();if(!saving)close();});
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(saving)return;
    const name=input.value.trim();input.setCustomValidity(name?'':'Введите название записи');
    if(!form.reportValidity())return;
    if(name===entry.name){close();return;}
    saving=true;saveButton.disabled=true;cancelButton.disabled=true;input.disabled=true;saveButton.textContent='Сохраняем…';
    try{
      const updated=await call<Entry>('rename_history',{id,name});
      history=history.map(item=>item.id===id?updated:item);
      if(active?.id===id)active={...active,name:updated.name};
      const scroll=document.querySelector<HTMLElement>('.transcript-scroll,.page-scroll'),scrollTop=scroll?.scrollTop||0;
      close();render();
      const restoredScroll=document.querySelector<HTMLElement>('.transcript-scroll,.page-scroll');if(restoredScroll)restoredScroll.scrollTop=scrollTop;
      if(view==='history')document.querySelector<HTMLInputElement>('#history-search')?.focus();
      notice('Название изменено');
    }catch(error){
      errorText.textContent=`Не удалось переименовать запись: ${String(error)}`;errorText.hidden=false;
      saving=false;saveButton.disabled=false;cancelButton.disabled=false;input.disabled=false;saveButton.textContent='Сохранить';input.focus();
    }
  });
  dialog.showModal();input.focus();input.select();
}
async function deleteEntry(id:string) {
  if(busy)return;const entry=history.find(item=>item.id===id);if(!entry)return;
  if(!await confirmAction('Удалить запись?',`«${entry.name}» и её расшифровка будут удалены из архива. Исходный файл останется на месте.`,'Удалить запись'))return;
  try{await call('delete_history',{id});history=history.filter(item=>item.id!==id);if(active?.id===id){resetPlayback();active=null;text='';resultSaved=false;previousResult=null;}render();}
  catch(error){notice(`Не удалось удалить запись: ${String(error)}`,true);}
}
async function clearHistory() {
  if(busy||!await confirmAction('Очистить архив?',`Будут удалены все записи из архива (${recordCount(history.length)}) и их расшифровки. Исходные файлы останутся на месте.`,'Очистить архив'))return;
  try{await call('clear_history');resetPlayback();history=[];active=null;text='';resultSaved=false;previousResult=null;render();}
  catch(error){notice(`Не удалось очистить архив: ${String(error)}`,true);}
}
async function download(id:string) {
  if(busy)return;beginOperation('download','Скачиваем модель');render();
  try{await call('download_model',{id});const statuses=await call<{id:string;installed:boolean}[]>('model_status');models.forEach(model=>model.installed=statuses.find(item=>item.id===model.id)?.installed||false);}
  catch(error){status=`Не удалось скачать модель: ${String(error)}`;isError=true;}
  finally{endOperation();render();}
}
async function run() {
  if(!active||busy||readiness())return;
  const id=active.id,beforeAttempt={text,saved:resultSaved};
  if(text&&resultSaved)previousResult={text,saved:true};
  receivedPartial=false;
  beginOperation('transcribe','Подготавливаем запись');render();
  try {
    await settingsSaveQueue;if(!await persist())throw new Error('Настройки распознавания не применены');
    text=await call<string>('transcribe',{id,model:settings.provider==='local'?settings.localModel:settings.openrouterModel});
    const detail=await call<{entry:Entry;text:string;warning?:string|null}>('get_history',{id});active=detail.entry;resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;
    history=sortHistory(await call<Entry[]>('list_history'));
  } catch(error) {
    if(!receivedPartial){text=beforeAttempt.text;resultSaved=beforeAttempt.saved;if(resultSaved)previousResult=null;}
    status=`Не удалось завершить распознавание: ${String(error)}`;isError=true;
  } finally {endOperation();render();}
}
function confirmAction(heading:string,message:string,action:string,danger=true):Promise<boolean> {
  if(document.querySelector('dialog[open]'))return Promise.resolve(false);
  return new Promise(resolve=>{
    const dialog=document.createElement('dialog');dialog.className='confirmation';dialog.setAttribute('aria-labelledby','confirmation-title');dialog.setAttribute('aria-describedby','confirmation-description');
    dialog.innerHTML=`<h2 id="confirmation-title">${esc(heading)}</h2><p id="confirmation-description">${esc(message)}</p><div><button class="secondary" data-cancel>Отмена</button><button class="${danger?'destructive':'primary'}" data-confirm>${esc(action)}</button></div>`;
    document.body.append(dialog);dialog.showModal();
    const finish=(value:boolean)=>{dialog.close();dialog.remove();resolve(value);};
    dialog.querySelector('[data-cancel]')?.addEventListener('click',()=>finish(false));dialog.querySelector('[data-confirm]')?.addEventListener('click',()=>finish(true));dialog.addEventListener('cancel',event=>{event.preventDefault();finish(false);});dialog.querySelector<HTMLButtonElement>('[data-cancel]')?.focus();
  });
}

render();
Promise.allSettled([call<Settings>('get_settings'),refreshHistory(),refreshRuntime()]).then(results=>{if(results[0].status==='fulfilled')settings=results[0].value;else notice(`Не удалось загрузить настройки: ${String(results[0].reason)}`,true);render();});
subscribe<Progress>('task-progress',payload=>{if(!busy)return;progress={...payload,elapsed:Math.max(payload.elapsed,Math.floor((Date.now()-operationStarted)/1000))};updateProgress();}).catch(error=>notice(String(error),true));
subscribe<{text:string}>('partial-text',payload=>{
  if(!busy||progress?.kind!=='transcribe'||!payload.text)return;
  receivedPartial=true;text=payload.text;resultSaved=false;
  const result=document.querySelector('#result');if(result){const follow=result.scrollHeight-result.scrollTop-result.clientHeight<90;result.innerHTML=outputHtml();if(follow)result.scrollTop=result.scrollHeight;}
  document.querySelectorAll<HTMLButtonElement>('#copy,#export').forEach(element=>element.disabled=!text);
  const meta=document.querySelector('#text-meta');if(meta)meta.textContent=resultMeta();
}).catch(error=>notice(String(error),true));
subscribeAudioDrop(paths=>{if(paths.length){if(paths.length>1)notice('Добавляем первый файл. Остальные можно добавить по одному.');choose(paths[0]);}}).catch(error=>notice(String(error),true));
if(preview){
  document.addEventListener('dragover',event=>{event.preventDefault();if(!busy)document.querySelector('.main-surface')?.classList.add('drop-hover');});
  document.addEventListener('dragleave',event=>{if(!event.relatedTarget)document.querySelector('.main-surface')?.classList.remove('drop-hover');});
  document.addEventListener('drop',event=>{event.preventDefault();document.querySelector('.main-surface')?.classList.remove('drop-hover');const file=event.dataTransfer?.files[0];if(file&&!busy){if(active)notice('Для другого файла создайте новую запись.');else choose(registerPreviewFile(file));}});
}
document.addEventListener('keydown',event=>{
  if(document.querySelector('dialog[open]'))return;
  if(!(event.metaKey||event.ctrlKey))return;
  const key=event.key.toLowerCase();
  if(key==='o'){event.preventDefault();if(!busy)choose();}
  if(key==='n'){event.preventDefault();if(!busy)newRecord();}
  if(key==='k'){event.preventDefault();changeView('history');}
  if(key==='enter'&&view==='work'){event.preventDefault();run();}
  if(key==='b'){event.preventDefault();setSidebar(!sidebarCollapsed);}
});
window.addEventListener('beforeunload',event=>{if(settingsDraft||busy||(text&&!resultSaved))event.preventDefault();});
