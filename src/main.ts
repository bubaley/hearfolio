import './styles.css';
import {call, pickAudio, exportText, subscribe, subscribeAudioDrop, registerPreviewFile, audioSource, openExternal, pickStorageParent, loadRuntimePlatform, previewCapabilities, appVersion, checkDesktopUpdate, latestAndroidRelease, restartApp, isNewerVersion, preview, type Entry, type Progress, type Settings, type CloudModel, type RecognitionConfig, type RuntimeCapabilities, type AndroidRelease, type AppUpdate} from './api';
import {icon, escapeHtml as esc} from './icons';
import {tr, locale, countLabel, errorText, setLanguage, currentLanguage, type LanguagePreference} from './i18n';

type View = 'work' | 'history' | 'models' | 'settings';
type SettingsDraft = {url:string;token:string;language:LanguagePreference};
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
let platform:RuntimeCapabilities=preview?{...previewCapabilities}:{os:'unknown',mobile:false,localRecognition:false,customStorage:false,nativeAudio:false};
let applicationVersion='',updateChecking=false,updateChecked=false,updateError='',pendingUpdate:AppUpdate|null=null,androidRelease:AndroidRelease|null=null,updateInstalled=false,installedUpdateVersion='',lastNotifiedUpdate='',lastUpdateCheck=0;
let configuration:RecognitionConfig={...settings.lastConfiguration};
let localChoice='whisper-base',cloudChoice='',catalogOpen=false,catalogSearch='',catalogError='';
let configurationSaveQueue:Promise<unknown>=Promise.resolve();
let settingsDraft:SettingsDraft|null=null, cloudModels:CloudModel[]=[];
let runtime={ffmpeg:false,whisper:false,nemo:false};
let runtimeLoaded=false, historyError='', sidebarCollapsed=localStorage.getItem('hearfolio-sidebar')==='collapsed'||(!localStorage.getItem('hearfolio-sidebar')&&window.innerWidth<=760);
let theme=localStorage.getItem('hearing-theme')||(matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light');
let operationStarted=0, elapsedTimer:ReturnType<typeof setInterval>|null=null;
let settingsSaveQueue:Promise<unknown>=Promise.resolve(), previousResult:{text:string;saved:boolean}|null=null;
let receivedPartial=false, audioPosition=0, audioWasPlaying=false, pickingAudio=false;
let noticeTimer:ReturnType<typeof setTimeout>|null=null;
document.documentElement.dataset.theme=theme;
const title:Record<View,string>={work:"Расшифровка",history:"Записи",models:"Модели",settings:"Настройки"};
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
  if (configuration.provider==='openrouter') return !settings.hasToken?tr("Добавьте API token OpenRouter."):cloudLoading?tr("Загружаем модели…"):catalogError?tr("Обновите каталог моделей."):!cloudModels.some(model=>model.id===configuration.model&&model.modes.includes(configuration.mode as 'streaming'|'transcription'))?tr("Выберите доступную модель OpenRouter."):'';
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
function endOperation() {busy=false;progress=null;if(elapsedTimer)clearInterval(elapsedTimer);elapsedTimer=null;}
function progressHtml():string {
  if (!busy||!progress) return '';
  const p=progress,percent=p.total?Math.min(100,Math.floor(p.current/p.total*100)):null;
  const remaining=p.elapsed>0&&p.current>0&&p.total>p.current?Math.ceil((p.total-p.current)/(p.current/p.elapsed)):0;
  return `<div class="task-progress" role="status"><div class="progress-label"><span class="spinner"></span><strong>${esc(p.stage)}</strong><span>${percent===null?'':`${percent}%`}</span></div><div class="track ${percent===null?'indeterminate':''}"><i style="width:${percent??25}%"></i></div><div class="progress-detail"><span>${(p.kind==='download'||p.kind==='update')&&p.total?`${(p.current/1048576).toFixed(1)} / ${(p.total/1048576).toFixed(1)}${tr(" МБ · ")}`:''}${tr("Прошло ")}${formatTime(p.elapsed)}</span><span>${remaining?`${tr("Осталось около ")}${formatTime(remaining)}`:tr("Оцениваем время")}</span></div></div>`;
}
function updateProgress() {const element=document.querySelector('#progress');if(element)element.innerHTML=progressHtml();}
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
  return `${tr("<aside class=\"sidebar\" aria-label=\"Навигация\"><div class=\"sidebar-chrome\" data-tauri-drag-region><button class=\"icon-button\" id=\"collapse\" title=\"Свернуть боковую панель\" aria-label=\"Свернуть боковую панель\" aria-expanded=\"true\">")}${icon('panel')}${tr("</button></div><div class=\"sidebar-body\"><div class=\"brand-row\"><button class=\"brand\" data-view=\"work\" title=\"Hearfolio — расшифровка аудио\">")}${icon('wave')}Hearfolio</button></div><div class="primary-nav"><button class="nav-item" id="new" ${disabled()}>${icon('plus')}${tr("<span>Новая запись</span><kbd>⌘ N</kbd></button><button class=\"nav-item ")}${view==='history'?'active':''}" data-view="history">${icon('search')}${tr("<span>Найти запись</span><kbd>⌘ K</kbd></button></div><div class=\"recent-records\"><div class=\"recent-heading\"><span>Недавние записи</span>")}${history.length?`${tr("<button class=\"text-button\" data-view=\"history\">Все ")}${history.length}</button>`:''}</div>${loading?tr("<div class=\"sidebar-empty\">Загружаем записи…</div>"):historyError?tr("<button class=\"text-button\" id=\"retry-history\">Повторить загрузку</button>"):recent.length?historyItems(recent,true):tr("<div class=\"sidebar-empty\">Здесь появятся ваши записи</div>")}</div><nav class="bottom-nav" aria-label="${tr('Приложение')}">${desktopModelsNav()}<button class="nav-item ${view==='settings'?'active':''}" data-view="settings">${icon('settings')}${tr("<span>Настройки</span></button></nav><div class=\"sidebar-footer\"><span>")}${icon('headphones')}${tr("Личный аудиоархив</span><button class=\"icon-button\" data-theme title=\"")}${theme==='dark'?tr("Светлая тема"):tr("Тёмная тема")}" aria-label="${theme==='dark'?tr("Светлая тема"):tr("Тёмная тема")}">${icon(theme==='dark'?'sun':'moon')}</button></div></div></aside>`;
}
function outputHtml():string {
  if (text) return `<article class="transcript">${esc(text)}</article>`;
  if (active) return `<div class="record-empty"><div class="empty-symbol">${icon('wave')}</div><h2>${busy&&progress?.kind==='transcribe'?tr("Распознаём запись…"):tr("Запись готова к распознаванию")}</h2><p>${busy&&progress?.kind==='transcribe'?tr("Текст будет появляться здесь по мере обработки."):tr("Выберите способ распознавания внизу и нажмите «Распознать».")}</p></div>`;
  return `<div class="welcome" id="drop-area"><div class="empty-symbol">${icon('wave')}${tr("</div><h1>Превратите запись в текст</h1><p>Встреча, интервью или голосовая заметка —<br>добавьте аудиофайл, чтобы начать.</p><button class=\"secondary choose-file\" data-choose ")}${disabled()}>${icon('plus')}${tr("Выбрать аудиофайл<kbd>⌘ O</kbd></button><small>")}${platform.mobile?'':tr("Или перетащите файл сюда")+' · '}${audioFormats}</small></div>`;
}
function resultMeta():string {return busy&&progress?.kind==='transcribe'?tr("Распознавание…"):text?`${countLabel(words(),'word')} · ${resultSaved?tr("Сохранено в архиве"):tr("Неполный текст · не сохранён")}`:tr("Аудиофайл");}
function providerControlsHtml():string {
  if(!platform.localRecognition)return `<span class="mobile-provider">${icon('cloud')}OpenRouter</span><span class="control-divider"></span>${cloudControlsHtml()}`;
  return `${tr("<label class=\"control-select provider-select\"><span class=\"sr-only\">Способ распознавания</span>")}${icon(configuration.provider==='local'?'headphones':'cloud')}<select id="provider" ${disabled()}><option value="local" ${configuration.provider==='local'?'selected':''}${tr(">На устройстве</option><option value=\"openrouter\" ")}${configuration.provider==='openrouter'?'selected':''}>OpenRouter</option></select></label><span class="control-divider"></span>${configuration.provider==='local'?`${tr("<label class=\"control-select model-select\"><span class=\"sr-only\">Локальная модель</span><select id=\"local-model\" ")}${disabled()}>${models.map(model=>`<option value="${model.id}" ${configuration.model===model.id?'selected':''}>${model.name}${model.installed?'':tr(" · скачать")}</option>`).join('')}</select></label>`:cloudControlsHtml()}`;
}
function cloudControlsHtml():string {return `<div class="cloud-picker"><button type="button" class="cloud-model-control" id="cloud-picker-toggle" aria-haspopup="dialog" aria-expanded="${catalogOpen}" ${disabled()}><span>${esc(cloudModels.find(model=>model.id===configuration.model)?.name||configuration.model||tr("Выбрать модель"))}</span>${icon('chevron')}</button>${cloudModels.some(model=>model.id===configuration.model)?`<small class="recognition-mode">${configuration.mode==='transcription'?tr("Аудиотранскрибация"):tr("Потоковый текст")}</small>`:''}${catalogOpen?catalogHtml():''}</div>`;}
function catalogHtml():string {
  const matches=cloudModels.filter(model=>`${model.name} ${model.id}`.toLocaleLowerCase().includes(catalogSearch.toLocaleLowerCase()));
  return `${tr("<section class=\"model-catalog\" role=\"dialog\" aria-label=\"Модели OpenRouter\"><div class=\"catalog-heading\"><strong>Модели OpenRouter</strong><button type=\"button\" class=\"icon-button\" id=\"catalog-close\" aria-label=\"Закрыть\">")}${icon('close')}</button></div><label class="search-field">${icon('search')}${tr("<input type=\"search\" id=\"catalog-search\" placeholder=\"Найти модель\" value=\"")}${esc(catalogSearch)}${tr("\" aria-label=\"Найти модель\"></label><div id=\"catalog-results\">")}${catalogResults(matches)}</div><button type="button" class="text-button" id="catalog-refresh" ${cloudLoading?'disabled':''}${tr(">Обновить каталог</button></section>")}`;
}
function catalogResults(matches:CloudModel[]):string {
  if(cloudLoading)return tr("<div class=\"catalog-empty\"><span class=\"spinner\"></span> Загружаем модели…</div>");
  if(catalogError)return `<div class="catalog-empty" role="alert">${esc(catalogError)}</div>`;
  if(!matches.length)return `<div class="catalog-empty">${cloudModels.length?tr("Модели не найдены"):tr("Нет моделей с поддержкой аудио")}</div>`;
  return `<div class="catalog-list">${matches.map(model=>`<button type="button" class="catalog-option ${configuration.model===model.id?'selected':''}" data-cloud-model="${esc(model.id)}"><span><strong>${esc(model.name)}</strong><small>${esc(model.id)}</small></span>${configuration.model===model.id?icon('check'):''}</button>`).join('')}</div>`;
}
function runButtonHtml(issue:string):string {
  const label=resultSaved?tr("Распознать ещё раз"):tr("Распознать");
  return `<button class="run-button ${active&&!busy?'with-label':''}" id="run" ${busy||!active||issue?'disabled':''} title="${esc(issue||(!active?tr("Добавьте аудиофайл"):label+' · ⌘ Enter'))}" aria-label="${label}">${active&&!busy?`<span>${label}</span>`:''}${busy&&progress?.kind==='transcribe'?'<span class="spinner"></span>':icon('arrow')}</button>`;
}
function composerHtml(issue:string):string {
  const needsDownload=configuration.provider==='local'&&runtime.ffmpeg&&!models.find(model=>model.id===localChoice)?.installed;
  const needsCatalog=configuration.provider==='openrouter'&&(platform.nativeAudio||runtime.ffmpeg)&&settings.hasToken;
  const action=needsDownload?`data-download="${localChoice}"`:needsCatalog?'id="open-catalog"':configuration.provider==='openrouter'&&(platform.nativeAudio||runtime.ffmpeg)?'data-view="settings"':'data-view="models"';
  return `<div class="composer-area"><div class="composer ${active?'has-record':''}">
    <div id="progress">${progressHtml()}</div>
    ${!active?`<div class="composer-top"><button class="attachment" data-choose ${disabled()}>${icon('plus')}${tr("<span>Добавить аудиофайл</span><kbd>⌘ O</kbd></button>")}${runButtonHtml(issue)}</div>`:''}
    <div class="composer-controls">${providerControlsHtml()}${active?runButtonHtml(issue):''}</div>
    ${issue?`<div class="composer-requirement"><span>${esc(issue)}</span>${runtimeLoaded?`<button class="text-button" ${action} ${disabled()}>${needsDownload?tr("Скачать модель"):needsCatalog?tr('Выбрать модель'):tr("Настроить")}${icon('chevron')}</button>`:''}</div>`:''}
  </div></div>`;
}
function workHtml():string {
  const issue=readiness(),source=active?audioSource(active):'';
  return `<section class="workspace">
    ${active?`<div class="record-toolbar"><div class="record-info">${icon('wave')}<div><div class="record-name-row"><h1 title="${esc(active.name)}">${esc(active.name)}</h1><button class="icon-button" data-rename="${esc(active.id)}${tr("\" aria-label=\"Переименовать запись\" title=\"Переименовать запись\" ")}${disabled()}>${icon('edit')}</button></div><span id="text-meta">${resultMeta()}${tr("</span></div></div><div class=\"output-actions\"><button class=\"icon-button\" id=\"copy\" title=\"Копировать текст\" aria-label=\"Копировать текст\" ")}${!text?'disabled':''}>${icon('copy')}${tr("</button><button class=\"icon-button\" id=\"export\" title=\"Экспортировать .txt\" aria-label=\"Экспортировать текст\" ")}${!text?'disabled':''}>${icon('download')}</button></div></div>${source?`<div class="audio-bar"><audio id="audio" controls preload="metadata" src="${esc(source)}${tr("\" aria-label=\"Прослушать ")}${esc(active.name)}"></audio><span>${dateLabel(active.createdAt)}${active.sizeBytes?` · ${(active.sizeBytes/1048576).toFixed(1)}${tr(" МБ")}`:''}</span></div>`:''}`:''}
    <div class="transcript-scroll" id="result">${outputHtml()}</div>
    ${previousResult?.saved&&!busy&&!resultSaved?tr("<div class=\"restore-result\"><span>Предыдущая расшифровка сохранена в архиве.</span><button class=\"text-button\" id=\"restore\">Вернуть предыдущий текст</button></div>"):''}
    ${composerHtml(issue)}
  </section>`;
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
function mobileAppearanceHtml():string {
  if(!platform.mobile)return '';
  return `<section class="settings-section"><h2>${tr('Внешний вид')}</h2><div class="setting-row"><div><strong>${tr('Тема приложения')}</strong><p>${tr(theme==='dark'?'Тёмная тема':'Светлая тема')}</p></div><button type="button" class="secondary" data-theme>${icon(theme==='dark'?'sun':'moon')}${tr(theme==='dark'?'Светлая тема':'Тёмная тема')}</button></div></section>`;
}
function storageSettingsHtml():string {
  return `<section class="settings-section"><h2>${tr('Ваш архив')}</h2><p>${platform.customStorage?tr('Выберите родительскую папку. Hearfolio создаст внутри неё папку ')+`<code>.hearfolio</code>`+tr(' и безопасно перенесёт текущий архив.'):tr('На Android архив хранится в защищённой папке приложения. Выбор другой папки пока недоступен.')}</p>${platform.customStorage?`<div class="archive-path"><span>${tr('Родительская папка')}</span><code>${esc(settings.storageParent||tr('Определяем расположение…'))}</code></div>`:''}<div class="archive-path"><span>${tr('Архив')}</span><code>${esc(settings.storagePath||tr('Определяем расположение…'))}</code></div>${platform.customStorage?`<button type="button" class="secondary" id="choose-storage" ${disabled()}>${tr('Выбрать папку')}</button>`:''}<small class="storage-help">${tr('Исходные аудиофайлы остаются на своих местах.')}</small></section>`;
}
function settingsHtml():string {
  const draft=settingsDraft||{url:settings.openrouterUrl,token:'',language:settings.language};
  return `<div class="page-heading"><div><h1>${tr('Настройки')}</h1><p>${tr('Язык, подключение и расположение вашего архива.')}</p></div></div><form id="settings-form" class="settings-panel"><section class="settings-section"><h2>${tr('Язык приложения')}</h2><label class="field"><span>${tr('Язык')}</span><select name="language"><option value="system" ${draft.language==='system'?'selected':''}>${tr('Системный')}</option><option value="ru" ${draft.language==='ru'?'selected':''}>${tr('Русский')}</option><option value="en" ${draft.language==='en'?'selected':''}>English</option></select><small>${tr('Системный язык определяется по настройкам устройства.')}</small></label></section><section class="settings-section"><div class="section-heading"><h2>OpenRouter</h2><span class="badge ${settings.hasToken?'success':''}">${settings.hasToken?tr('Токен подключён'):tr('Нужен API token')}</span></div><p>${tr('Аудио отправляется выбранному провайдеру. Стоимость и обработка данных зависят от модели.')}</p><label class="field"><span>API URL</span><input name="url" type="url" required value="${esc(draft.url)}" placeholder="https://openrouter.ai/api/v1"><small>${tr('Измените адрес для другого совместимого сервиса.')}</small></label><label class="field"><span>API token</span><input name="token" type="password" autocomplete="off" value="${esc(draft.token)}" placeholder="${settings.hasToken?tr('Введите новый токен для замены'):'sk-or-…'}"><small>${settings.hasToken?tr('Оставьте поле пустым, чтобы использовать сохранённый токен.'):tr('Токен хранится в настройках приложения на этом устройстве.')}</small></label>${settings.hasToken?`<button type="button" id="remove-token" class="text-button danger">${tr('Удалить сохранённый токен')}</button>`:''}</section>${storageSettingsHtml()}${mobileAppearanceHtml()}<div class="settings-actions"><button class="primary" type="submit" ${busy||!settingsDraft?'disabled':''}>${tr('Применить')}</button><span>${settingsDraft?tr('Есть неприменённые изменения'):''}</span></div></form><section class="settings-section about-section"><h2>${tr('О приложении')}</h2><div class="app-version"><strong>Hearfolio</strong><span>${tr('Версия')} ${esc(applicationVersion||'…')}</span></div><div id="about-updates">${updatesHtml()}</div></section>`;
}
function mobileNavHtml():string {
  return `<nav class="mobile-nav" aria-label="${tr('Навигация')}">${(['work','history',...(platform.localRecognition?['models']:[]),'settings'] as View[]).map(target=>`<button type="button" data-view="${target}" class="${view===target?'active':''}" ${target==='work'&&busy?'disabled':''}>${icon(target==='work'?'wave':target==='history'?'history':target)}<span>${tr(title[target])}</span></button>`).join('')}</nav>`;
}
function updateHeaderHtml():string {
  return pendingUpdate||androidRelease||updateInstalled?`<button type="button" class="text-button update-shortcut" data-view="settings">${icon('download')}<span>${tr(updateInstalled?'Нужен перезапуск':'Доступно обновление')}</span></button>`:'';
}
function updatesHtml():string {
  if(platform.os!=='android'&&platform.os!=='macos'&&platform.os!=='linux')return `<p>${tr('Обновления на этой платформе недоступны.')}</p>`;
  if(updateInstalled)return `<p>${tr('Обновление установлено. Перезапустите приложение, когда закончите работу.')}</p><button type="button" class="secondary" id="restart-update" ${busy?'disabled':''}>${tr('Перезапустить приложение')}</button>`;
  const version=pendingUpdate?.version||androidRelease?.version;
  return `<p>${platform.mobile?tr('На Android обновление устанавливается вручную из APK. Ваш архив останется в приложении.'):tr('Обновления проверяются автоматически. Установка и перезапуск выполняются по вашему запросу.')}</p>${version?`<div class="update-offer"><span>${tr('Доступно обновление')} <strong>${esc(version)}</strong></span><button type="button" class="secondary" id="install-update" ${busy||updateChecking?'disabled':''}>${tr(platform.mobile?'Открыть APK':'Установить обновление')}</button></div>`:''}${updateError?`<p class="update-error" role="alert">${esc(tr(updateError))}</p>`:updateChecked&&!version?`<p class="update-checked">${tr('Установлена последняя версия')}</p>`:''}<button type="button" class="text-button" id="check-update" ${busy||updateChecking?'disabled':''}>${tr(updateChecking?'Проверяем обновления…':'Проверить обновления')}</button>`;
}
function refreshUpdateUi() {
  const section=document.querySelector('#about-updates');if(section){section.innerHTML=updatesHtml();bindUpdates();}
  const header=document.querySelector('#update-header');if(header){header.innerHTML=updateHeaderHtml();header.querySelector<HTMLElement>('[data-view]')?.addEventListener('click',()=>changeView('settings'));}
}
async function checkUpdates(manual=false) {
  if(busy||updateChecking||updateInstalled||(!platform.mobile&&platform.os!=='macos'&&platform.os!=='linux')||platform.os==='ios')return;
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
async function restartAfterUpdate() {
  if(busy||!updateInstalled)return;
  if(!await mayLeaveSettings()||!await mayReplaceRecord())return;
  if(!await confirmAction(tr('Перезапустить приложение?'),tr('Текущая работа будет завершена. Сохранённые записи останутся в архиве.'),tr('Перезапустить'),false))return;
  if(busy)return;
  busy=true;render();
  try{await restartApp();if(preview){applicationVersion=installedUpdateVersion;updateInstalled=false;busy=false;render();}}catch(error){busy=false;render();notice(`${tr('Не удалось перезапустить приложение: ')}${errorText(error)}`,true);}
}
function bindUpdates() {
  document.querySelector('#check-update')?.addEventListener('click',()=>checkUpdates(true));
  document.querySelector('#install-update')?.addEventListener('click',installUpdate);
  document.querySelector('#restart-update')?.addEventListener('click',restartAfterUpdate);
}
function preservePlayback() {
  const audio=document.querySelector<HTMLAudioElement>('#audio');
  if (audio) {audioPosition=audio.currentTime;audioWasPlaying=!audio.paused;}
}
function render() {
  preservePlayback();
  document.documentElement.dataset.platform=platform.os;
  app.innerHTML=`<div class="shell ${sidebarCollapsed?'sidebar-collapsed':''} ${platform.mobile?'mobile-platform':''}">${sidebarHtml()}<main><header class="main-header" data-tauri-drag-region><div class="header-title" data-tauri-drag-region>${!platform.mobile?`${tr("<button class=\"icon-button\" id=\"expand\" title=\"Показать боковую панель\" aria-label=\"Показать боковую панель\" aria-expanded=\"false\">")}${icon('panel')}</button>`:''}<span data-tauri-drag-region>${esc(view==='work'?(active?.name||tr("Новая расшифровка")):tr(title[view]))}</span></div><button type="button" class="icon-button mobile-new" id="mobile-new" aria-label="${tr('Новая запись')}" ${disabled()}>${icon('plus')}</button><div class="header-status"><div id="update-header">${updateHeaderHtml()}</div>${busy?`<span class="header-activity"><span class="spinner"></span>${progress?.kind==='download'?tr("Скачиваем модель"):progress?.kind==='import'?tr("Добавляем запись"):progress?.kind==='storage'?tr('Переносим архив'):progress?.kind==='update'?tr('Устанавливаем обновление'):tr("Распознаём")}</span>`:''}${preview?tr("<span class=\"preview-label\">Предпросмотр</span>"):''}</div></header><div class="main-surface ${view==='work'?'work-surface':''}">${view==='work'?workHtml():`<div class="page-scroll"><div class="page-content">${view==='history'?historyHtml():view==='models'?modelsHtml():settingsHtml()}${busy?`<div id="progress">${progressHtml()}</div>`:''}</div></div>`}<div id="status" class="notice ${isError?'error':''}" role="status" aria-live="polite"></div></div>${mobileNavHtml()}</main></div>`;
  bind();updateNotice();restorePlayback();
}
function restorePlayback() {
  const audio=document.querySelector<HTMLAudioElement>('#audio');
  if (!audio) return;
  audio.addEventListener('loadedmetadata',()=>{if(audioPosition&&audioPosition<audio.duration)audio.currentTime=audioPosition;if(audioWasPlaying)audio.play().catch(()=>{});});
  audio.addEventListener('error',()=>{const bar=document.querySelector('.audio-bar');if(bar)bar.innerHTML=tr("<span class=\"playback-error\">Не удалось воспроизвести этот формат. Файл можно распознать.</span>");});
}
function resetPlayback() {audioPosition=0;audioWasPlaying=false;const audio=document.querySelector<HTMLAudioElement>('#audio');if(audio){audio.pause();audio.currentTime=0;}}
async function mayReplaceRecord() {return !text||resultSaved||await confirmAction(tr("Неполный текст не сохранён"),tr("Скопируйте или экспортируйте текст, чтобы сохранить его. При переходе к другой записи этот результат будет потерян."),tr("Продолжить без текста"),false);}
async function mayLeaveSettings() {return !settingsDraft||await confirmAction(tr("Изменения не применены"),tr("Примените настройки перед уходом или продолжите без этих изменений."),tr("Продолжить без изменений"),false);}
async function changeView(target:View) {
  if(target==='models'&&!platform.localRecognition)return;
  if (view==='settings'&&target!=='settings') {if(!await mayLeaveSettings())return;settingsDraft=null;}
  view=target;status='';if(window.innerWidth<=760)sidebarCollapsed=true;render();if(target==='history')document.querySelector<HTMLInputElement>('#history-search')?.focus();
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
  document.querySelector('#mobile-new')?.addEventListener('click',newRecord);
  document.querySelector('#run')?.addEventListener('click',run);
  document.querySelectorAll('button[data-theme]').forEach(element=>element.addEventListener('click',()=>{theme=theme==='dark'?'light':'dark';document.documentElement.dataset.theme=theme;localStorage.setItem('hearing-theme',theme);render();}));
  document.querySelector('#collapse')?.addEventListener('click',()=>setSidebar(true));document.querySelector('#expand')?.addEventListener('click',()=>setSidebar(false));
  document.querySelector<HTMLSelectElement>('#provider')?.addEventListener('change',event=>{const provider=(event.target as HTMLSelectElement).value as RecognitionConfig['provider'];chooseConfiguration(provider==='local'?{provider,model:localChoice,mode:'local'}:{provider,model:cloudChoice,mode:cloudModels.find(model=>model.id===cloudChoice)?.preferredMode||'streaming'});if(provider==='openrouter'&&!cloudModels.length)loadCloudModels();});
  document.querySelector<HTMLSelectElement>('#local-model')?.addEventListener('change',event=>{chooseConfiguration({provider:'local',model:(event.target as HTMLSelectElement).value,mode:'local'});});
  document.querySelectorAll<HTMLElement>('[data-select]').forEach(element=>element.onclick=()=>chooseConfiguration({provider:'local',model:element.dataset.select!,mode:'local'}));
  bindCatalog();
  document.querySelectorAll<HTMLElement>('[data-download]').forEach(element=>element.onclick=()=>download(element.dataset.download!));
  bindRecordActions();
  document.querySelector<HTMLInputElement>('#history-search')?.addEventListener('input',event=>{search=(event.target as HTMLInputElement).value;document.querySelector('#history-results')!.innerHTML=historyResults();bindRecordActions();document.querySelectorAll('[data-choose]').forEach(element=>element.addEventListener('click',()=>choose()));});
  document.querySelector('#clear')?.addEventListener('click',clearHistory);
  document.querySelector('#retry-history')?.addEventListener('click',()=>refreshHistory());
  document.querySelector('#retry-runtime')?.addEventListener('click',()=>refreshRuntime());
  document.querySelector('#copy-install')?.addEventListener('click',()=>copyText(platform.os==='linux'?'sudo apt install ffmpeg':'brew install ffmpeg whisper.cpp'));
  document.querySelector('#copy')?.addEventListener('click',()=>copyText(text));
  document.querySelector('#export')?.addEventListener('click',()=>exportText((active?.name||tr("Расшифровка")).replace(/\.(m4a|mp3|wav|mp4|aac|flac|ogg)$/i,'').replace(/[\/\\:\u0000-\u001f]/g,'-')+'.txt',text).catch(error=>notice(errorText(error),true)));
  document.querySelector('#restore')?.addEventListener('click',()=>{if(previousResult){text=previousResult.text;resultSaved=previousResult.saved;previousResult=null;render();}});
  document.querySelectorAll<HTMLAnchorElement>('a[href^="https://"]').forEach(anchor=>anchor.onclick=event=>{event.preventDefault();openExternal(anchor.href).catch(error=>notice(errorText(error),true));});
  bindSettings();bindUpdates();
}
function setSidebar(collapsed:boolean) {sidebarCollapsed=collapsed;localStorage.setItem('hearfolio-sidebar',collapsed?'collapsed':'expanded');render();}
async function copyText(value:string) {try{await navigator.clipboard.writeText(value);notice(tr("Скопировано"));}catch(error){notice(`${tr("Не удалось скопировать текст: ")}${errorText(error)}`,true);}}
function readSettingsDraft(form:HTMLFormElement):SettingsDraft {const data=new FormData(form);return {url:String(data.get('url')).trim(),token:String(data.get('token')).trim(),language:String(data.get('language')) as LanguagePreference};}
function bindSettings() {
  const form=document.querySelector<HTMLFormElement>('#settings-form');
  form?.addEventListener('input',()=>{settingsDraft=readSettingsDraft(form);const button=form.querySelector<HTMLButtonElement>('[type=submit]');if(button)button.disabled=busy;const hint=form.querySelector('.settings-actions span');if(hint)hint.textContent=tr("Есть неприменённые изменения");});
  form?.addEventListener('submit',async event=>{event.preventDefault();await applySettings();});
  document.querySelector('#remove-token')?.addEventListener('click',async()=>{if(await confirmAction(tr("Удалить API token?"),tr("Для распознавания через OpenRouter потребуется подключить токен снова."),tr("Удалить токен"))){if(await persist('')){cloudModels=[];render();}}});
  document.querySelector('#choose-storage')?.addEventListener('click',changeStorage);
}
async function applySettings():Promise<boolean> {
  const form=document.querySelector<HTMLFormElement>('#settings-form');if(!form||!form.reportValidity())return false;
  const draft=readSettingsDraft(form),connectionChanged=draft.url!==settings.openrouterUrl||Boolean(draft.token);
  if(!await persist(draft.token||undefined,{language:draft.language,openrouterUrl:draft.url}))return false;
  settingsDraft=null;if(connectionChanged){cloudModels=[];catalogError='';}render();notice(tr("Настройки применены"));return true;
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
  return model&&!model.modes.includes(candidate.mode as 'streaming'|'transcription')?{...candidate,mode:model.preferredMode}:candidate;
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
  document.querySelectorAll<HTMLElement>('[data-cloud-model]').forEach(element=>element.onclick=()=>{const model=cloudModels.find(item=>item.id===element.dataset.cloudModel);if(model){catalogOpen=false;chooseConfiguration({provider:'openrouter',model:model.id,mode:model.preferredMode});}});
}
function bindCatalog() {
  document.querySelector('#cloud-picker-toggle')?.addEventListener('click',()=>{catalogOpen=!catalogOpen;render();if(catalogOpen){document.querySelector<HTMLInputElement>('#catalog-search')?.focus();if(!cloudModels.length)void loadCloudModels();}});
  document.querySelector('#catalog-close')?.addEventListener('click',()=>{catalogOpen=false;render();document.querySelector<HTMLButtonElement>('#cloud-picker-toggle')?.focus();});
  document.querySelector('#open-catalog')?.addEventListener('click',()=>{catalogOpen=true;render();document.querySelector<HTMLInputElement>('#catalog-search')?.focus();if(!cloudModels.length)void loadCloudModels();});
  document.querySelector('#catalog-refresh')?.addEventListener('click',()=>loadCloudModels());
  document.querySelector<HTMLInputElement>('#catalog-search')?.addEventListener('input',event=>{catalogSearch=(event.target as HTMLInputElement).value;const matches=cloudModels.filter(model=>`${model.name} ${model.id}`.toLocaleLowerCase().includes(catalogSearch.toLocaleLowerCase()));document.querySelector('#catalog-results')!.innerHTML=catalogResults(matches);bindCatalogOptions();});
  bindCatalogOptions();
}
async function loadCloudModels() {
  if(cloudLoading||busy)return;
  cloudLoading=true;catalogError='';render();
  try{
    cloudModels=(await call<CloudModel[]>('list_openrouter_models')).filter(model=>model.modes.some(mode=>mode==='streaming'||mode==='transcription')).map(model=>({...model,preferredMode:model.modes.includes('transcription')?'transcription':'streaming'}));
    const normalized=normalizeCatalogConfiguration(configuration);
    if(normalized.mode!==configuration.mode)chooseConfiguration(normalized);
  }
  catch(error){catalogError=errorText(error);}
  finally{cloudLoading=false;render();if(catalogOpen)document.querySelector<HTMLInputElement>('#catalog-search')?.focus();}
}
function sortHistory(entries:Entry[]) {return entries.sort((left,right)=>right.createdAt-left.createdAt);}
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
  resetPlayback();settingsDraft=null;active=null;restoreConfiguration();text='';resultSaved=false;previousResult=null;status='';view='work';render();
}
async function choose(path?:string) {
  if(busy||pickingAudio||platform.os==='unknown')return;
  if(active){notice(tr("Для другого файла создайте новую запись."));return;}
  if(!await mayLeaveSettings()||!await mayReplaceRecord())return;
  pickingAudio=true;let importStarted=false;
  try {
    const selected=path?{path}:await pickAudio();if(!selected)return;
    if(busy||active){if(active)notice(tr("Для другого файла создайте новую запись."));return;}
    importStarted=true;beginOperation('import',tr("Добавляем запись"));settingsDraft=null;view='work';render();
    const entry=await call<Entry>('import_audio',{path:selected.path,...('name' in selected&&selected.name?{name:selected.name}:{}),configuration:{...(configuration.model.trim()?configuration:settings.lastConfiguration)}});resetPlayback();active=entry;restoreConfiguration(entry);text='';resultSaved=false;previousResult=null;
    settings=await call<Settings>('get_settings');
    history=sortHistory(await call<Entry[]>('list_history'));
  } catch(error) {notice(`${tr("Не удалось добавить запись: ")}${errorText(error)}`,true);}
  finally {pickingAudio=false;if(importStarted){endOperation();render();}}
}
async function loadHistory(id:string) {
  if(busy||!await mayLeaveSettings())return;
  if(active?.id===id){settingsDraft=null;view='work';render();return;}
  if(!await mayReplaceRecord())return;
  try {const detail=await call<{entry:Entry;text:string;warning?:string|null}>('get_history',{id});resetPlayback();active=detail.entry;restoreConfiguration(detail.entry);text=detail.text;resultSaved=Boolean(detail.entry.outputPath&&detail.text);previousResult=null;settingsDraft=null;view='work';status=detail.warning?errorText(detail.warning):'';isError=Boolean(detail.warning);render();}
  catch(error) {notice(`${tr("Не удалось открыть запись: ")}${errorText(error)}`,true);}
}
async function renameEntry(id:string) {
  if(busy||document.querySelector('dialog[open]'))return;
  const entry=history.find(item=>item.id===id);if(!entry)return;
  const dialog=document.createElement('dialog');dialog.className='confirmation rename-dialog';
  dialog.setAttribute('aria-labelledby','rename-title');
  dialog.innerHTML=`${tr("<form><h2 id=\"rename-title\">Переименовать запись</h2><label class=\"field\"><span>Название</span><input name=\"record-name\" value=\"")}${esc(entry.name)}${tr("\" maxlength=\"200\" required autocomplete=\"off\" aria-describedby=\"rename-error\"></label><p id=\"rename-error\" class=\"rename-error\" role=\"alert\" hidden></p><div class=\"rename-actions\"><button type=\"button\" class=\"secondary\" data-cancel>Отмена</button><button type=\"submit\" class=\"primary\">Сохранить</button></div></form>")}`;
  document.body.append(dialog);
  const input=dialog.querySelector<HTMLInputElement>('input')!,form=dialog.querySelector<HTMLFormElement>('form')!;
  const renameError=dialog.querySelector<HTMLElement>('#rename-error')!,saveButton=dialog.querySelector<HTMLButtonElement>('[type=submit]')!,cancelButton=dialog.querySelector<HTMLButtonElement>('[data-cancel]')!;
  let saving=false;
  const close=()=>{dialog.close();dialog.remove();};
  input.addEventListener('input',()=>{input.setCustomValidity('');renameError.hidden=true;});
  cancelButton.onclick=()=>{if(!saving)close();};
  dialog.addEventListener('cancel',event=>{event.preventDefault();if(!saving)close();});
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(saving)return;
    const name=input.value.trim();input.setCustomValidity(name?'':tr("Введите название записи"));
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
      if(view==='history')document.querySelector<HTMLInputElement>('#history-search')?.focus();
      notice(tr("Название изменено"));
    }catch(error){
      renameError.textContent=`${tr("Не удалось переименовать запись: ")}${errorText(error)}`;renameError.hidden=false;
      saving=false;saveButton.disabled=false;cancelButton.disabled=false;input.disabled=false;saveButton.textContent=tr("Сохранить");input.focus();
    }
  });
  dialog.showModal();input.focus();input.select();
}
async function deleteEntry(id:string) {
  if(busy)return;const entry=history.find(item=>item.id===id);if(!entry)return;
  if(!await confirmAction(tr("Удалить запись?"),`«${entry.name}${tr("» и её расшифровка будут удалены из архива. Исходный файл останется на месте.")}`,tr("Удалить запись")))return;
  try{await call('delete_history',{id});history=history.filter(item=>item.id!==id);if(active?.id===id){resetPlayback();active=null;text='';resultSaved=false;previousResult=null;}render();}
  catch(error){notice(`${tr("Не удалось удалить запись: ")}${errorText(error)}`,true);}
}
async function clearHistory() {
  if(busy||!await confirmAction(tr("Очистить архив?"),`${tr("Будут удалены все записи из архива (")}${recordCount(history.length)}${tr(") и их расшифровки. Исходные файлы останутся на месте.")}`,tr("Очистить архив")))return;
  try{await call('clear_history');resetPlayback();history=[];active=null;text='';resultSaved=false;previousResult=null;render();}
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
  receivedPartial=false;
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
  } finally {try{settings=await call<Settings>('get_settings');}catch{/* Keep known defaults when settings cannot be refreshed. */}endOperation();render();}
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

render();
async function initialize() {
  const initial=await Promise.allSettled([loadRuntimePlatform(),call<Settings>('get_settings'),appVersion()]);
  if(initial[0].status==='fulfilled')platform=initial[0].value;else notice(errorText(initial[0].reason),true);
  if(initial[1].status==='fulfilled'){settings=initial[1].value;applyLanguagePreference();}else notice(`${tr('Не удалось загрузить настройки: ')}${errorText(initial[1].reason)}`,true);
  if(initial[2].status==='fulfilled')applicationVersion=initial[2].value;
  if(['macos','linux','windows'].includes(platform.os))subscribeAudioDrop(paths=>{if(paths.length){if(paths.length>1)notice(tr("Добавляем первый файл. Остальные можно добавить по одному."));choose(paths[0]);}}).catch(error=>notice(errorText(error),true));
  restoreConfiguration(active);render();
  await Promise.allSettled([refreshHistory(),refreshRuntime()]);
  if(!preview)void checkUpdates();
}
void initialize();
setInterval(()=>{if(!preview&&Date.now()-lastUpdateCheck>=4*60*60*1000)void checkUpdates();},4*60*60*1000);
document.addEventListener('visibilitychange',()=>{if(!preview&&document.visibilityState==='visible'&&Date.now()-lastUpdateCheck>=4*60*60*1000)void checkUpdates();});
subscribe<Progress>('task-progress',payload=>{if(!busy)return;progress={...payload,stage:errorText(payload.stage),elapsed:Math.max(payload.elapsed,Math.floor((Date.now()-operationStarted)/1000))};updateProgress();}).catch(error=>notice(errorText(error),true));
subscribe<{text:string}>('partial-text',payload=>{
  if(!busy||progress?.kind!=='transcribe'||!payload.text)return;
  if(!receivedPartial)void call<Settings>('get_settings').then(saved=>{settings=saved;}).catch(()=>{});receivedPartial=true;text=payload.text;resultSaved=false;
  const result=document.querySelector('#result');if(result){const follow=result.scrollHeight-result.scrollTop-result.clientHeight<90;result.innerHTML=outputHtml();if(follow)result.scrollTop=result.scrollHeight;}
  document.querySelectorAll<HTMLButtonElement>('#copy,#export').forEach(element=>element.disabled=!text);
  const meta=document.querySelector('#text-meta');if(meta)meta.textContent=resultMeta();
}).catch(error=>notice(errorText(error),true));
if(preview&&!previewCapabilities.mobile){
  document.addEventListener('dragover',event=>{event.preventDefault();if(!busy)document.querySelector('.main-surface')?.classList.add('drop-hover');});
  document.addEventListener('dragleave',event=>{if(!event.relatedTarget)document.querySelector('.main-surface')?.classList.remove('drop-hover');});
  document.addEventListener('drop',event=>{event.preventDefault();document.querySelector('.main-surface')?.classList.remove('drop-hover');const file=event.dataTransfer?.files[0];if(file&&!busy){if(active)notice(tr("Для другого файла создайте новую запись."));else choose(registerPreviewFile(file));}});
}
document.addEventListener('keydown',event=>{
  if(document.querySelector('dialog[open]'))return;
  if(event.key==='Escape'&&catalogOpen){catalogOpen=false;render();return;}
  if(!(event.metaKey||event.ctrlKey))return;
  const key=event.key.toLowerCase();
  if(key==='o'){event.preventDefault();if(!busy)choose();}
  if(key==='n'){event.preventDefault();if(!busy)newRecord();}
  if(key==='k'){event.preventDefault();changeView('history');}
  if(key==='enter'&&view==='work'){event.preventDefault();run();}
  if(key==='b'){event.preventDefault();setSidebar(!sidebarCollapsed);}
});
window.addEventListener('beforeunload',event=>{if(settingsDraft||busy||(text&&!resultSaved))event.preventDefault();});
