// Organizations (Hearfolio Cloud): connections in Settings, the recording
// destination, cloud recordings per organization and their detail screen.
// Rust owns the network, tokens, the upload queue and the audio cache; this
// module renders state and forwards user intent.
import './org.css';
import {convertFileSrc} from '@tauri-apps/api/core';
import {call as nativeCall, preview, subscribe, type Entry} from './api';
import {errorText, locale, currentLanguage} from './i18n';
import {icon, escapeHtml as esc} from './icons';
import {mergeRows, uploadStatus, recordingStatus, RETENTION_CHOICES, type UploadView, type CloudRecording, type Row, type Status} from './org-status';

type Organization={id:string;name:string;key:string;role:string};
export type Connection={id:string;server:string;serverName:string;serverVersion:string;user:{id:string;email:string;name:string};organizations:Organization[];createdAt:number;secure:boolean;needsReauth:boolean};
export type Destination={connectionId:string;organizationId:string;projectId:string|null;visibility:'organization'|'personal'};
type OrgState={connections:Connection[];settings:{destination:Destination|null;retentionDays:number|null};uploads:UploadView[];cache:{files:number;bytes:number}};
type Project={id:string;name:string;emoji?:string|null;is_private?:boolean};
type Result={id:string;title:string;status:string;content:string;model?:string;error_message?:string|null;updated_at?:string};
type Detail={recording:CloudRecording;results:Result[]|null;audioCached:boolean;offline:string|null};
type Library={items:CloudRecording[];syncedAt:number|null;loading:boolean;error:string};
type Context={render:()=>void;notice:(message:string,error?:boolean)=>void;confirm:(title:string,message:string,action:string,danger?:boolean)=>Promise<boolean>;openDetail:()=>void;closeDetail:()=>void;mobile:()=>boolean;android:()=>boolean;busy:()=>boolean;searchText:()=>string;openLocal:(entry:Entry)=>void};

const say=(ru:string,en:string)=>currentLanguage()==='ru'?ru:en;
let ctx:Context;
let state:OrgState={connections:[],settings:{destination:null,retentionDays:30},uploads:[],cache:{files:0,bytes:0}};
let loaded=false;
let source=localStorage.getItem('hearfolio-source')||'local';
const libraries=new Map<string,Library>();
const projects=new Map<string,Project[]>();
let open:{connectionId:string;organizationId:string;recordingId:string|null;clientId:string|null}|null=null;
let detail:Detail|null=null,detailLoading=false,detailFetching=false,detailError='',detailTab:'transcript'|'results'='transcript';
let audio:{key:string;src:string;loading:boolean;error:string;progress:number}|null=null;
let flow:{phase:'idle'|'checking'|'browser'|'exchanging';state:string;error:string}={phase:'idle',state:'',error:''};
let serverDraft='';

const libraryKey=(connectionId:string,organizationId:string)=>`${connectionId}:${organizationId}`;
function organization(connectionId:string,organizationId:string){const connection=state.connections.find(item=>item.id===connectionId);return {connection,organization:connection?.organizations.find(item=>item.id===organizationId)};}
function host(server:string){try{return new URL(server).host;}catch{return server;}}
const mb=(bytes:number)=>`${(bytes/1048576).toFixed(bytes<10485760?1:0)} ${say('МБ','MB')}`;
const timeLabel=(ms:number|null)=>{if(!ms)return '';const s=Math.floor(ms/1000),pad=(n:number)=>String(n).padStart(2,'0');return s>=3600?`${Math.floor(s/3600)}:${pad(Math.floor(s/60)%60)}:${pad(s%60)}`:`${Math.floor(s/60)}:${pad(s%60)}`;};
const dateLabel=(ms:number)=>ms?new Date(ms).toLocaleDateString(locale(),{day:'numeric',month:'long'}):'';
export function orgErrorText(error:unknown):string {
  const value=error instanceof Error?error.message:String(error),[code,detail]=value.split('|');
  const messages:Record<string,[string,string]>={
    'errors.orgUrlInvalid':['Введите адрес сервера, например cloud.example.com.','Enter a server address, e.g. cloud.example.com.'],
    'errors.orgUrlInsecure':['Для этого адреса нужен https://. Обычный http разрешён только в локальной сети.','This address needs https://. Plain http is allowed only on a local network.'],
    'errors.orgUnreachable':['Сервер не отвечает. Проверьте адрес и подключение к интернету.','The server does not respond. Check the address and your connection.'],
    'errors.orgNotHearfolio':['По этому адресу нет сервера Hearfolio.','There is no Hearfolio server at this address.'],
    'errors.orgAppOutdated':[`Сервер требует более новую версию Hearfolio${detail?` (${detail} или новее)`:''}. Обновите приложение.`,`The server requires a newer Hearfolio${detail?` (${detail} or later)`:''}. Update the app.`],
    'errors.orgServerOutdated':['Сервер устарел для этой версии приложения. Попросите администратора обновить его.','The server is too old for this app. Ask its administrator to update it.'],
    'errors.orgDiscovery':['Сервер вернул неверные настройки входа.','The server returned invalid sign-in settings.'],
    'errors.orgLoopback':['Не удалось подготовить вход. Повторите попытку.','Could not prepare sign-in. Try again.'],
    'errors.orgBrowser':['Не удалось открыть браузер для входа.','Could not open the browser to sign in.'],
    'errors.orgAuthExpired':['Вход не завершён или устарел. Начните заново.','Sign-in did not finish or has expired. Start again.'],
    'errors.orgAuthFailed':['Сервер отклонил вход. Начните заново.','The server rejected the sign-in. Start again.'],
    'errors.orgOffline':['Нет связи с сервером. Покажем сохранённое; обновим, когда сеть появится.','No connection to the server. Showing saved data; it will refresh when online.'],
    'errors.orgUnauthorized':['Доступ отозван. Войдите в организацию снова.','Access was revoked. Sign in to the organization again.'],
    'errors.orgForbidden':['Недостаточно прав для этого действия.','You do not have permission for this.'],
    'errors.orgNotFound':['Запись не найдена на сервере.','The recording was not found on the server.'],
    'errors.orgRecordingDeleted':['Запись удалена на сервере.','The recording was deleted on the server.'],
    'errors.orgConflict':['Конфликт на сервере. Повторите позже.','Server conflict. Try again later.'],
    'errors.orgTooLarge':['Файл слишком большой для сервера.','The file is too large for the server.'],
    'errors.orgRateLimited':['Слишком много попыток. Подождите немного.','Too many attempts. Wait a moment.'],
    'errors.orgServer':['Сервер временно недоступен. Повторим автоматически.','The server is temporarily unavailable. We will retry automatically.'],
    'errors.orgResponse':['Сервер ответил неожиданно. Повторите позже.','Unexpected server response. Try again later.'],
    'errors.orgRequest':['Сервер не принял запрос.','The server did not accept the request.'],
    'errors.orgMissing':['Организация больше не подключена.','The organization is no longer connected.'],
    'errors.orgDisconnected':['Подключение удалено. Сохраните запись на устройстве или удалите её.','The connection was removed. Keep the recording on this device or delete it.'],
    'errors.orgAudioMissing':['Аудиофайл для загрузки не найден.','The audio file to upload is missing.'],
    'errors.orgAudioChanged':['Аудиофайл изменился во время загрузки.','The audio file changed during upload.'],
    'errors.orgUploadRejected':['Сервер не принял файл. Загрузим заново.','The server rejected the file. Uploading again.'],
    'errors.orgUploadExpired':['Ссылка для загрузки устарела. Повторим.','The upload link expired. Retrying.'],
    'errors.orgSecretStore':['Не удалось сохранить ключ доступа в защищённом хранилище.','Could not save the access key in secure storage.'],
    'errors.orgStore':['Не удалось сохранить данные на устройстве.','Could not save data on this device.'],
    'errors.orgInvalid':['Неверные данные.','Invalid data.'],
  };
  const known=messages[code];
  return known?known[currentLanguage()==='ru'?0:1]:errorText(error);
}

// ---------------------------------------------------------------- preview fixtures
// `?org=1` in the Vite preview: a connected demo organization with recordings in every state.
const previewOrg=preview&&new URLSearchParams(location.search).get('org')==='1';
let previewState:OrgState|null=null;
function previewFixture():OrgState {
  const now=Date.now(),iso=(ms:number)=>new Date(ms).toISOString();
  const connection:Connection={id:'conn-demo',server:'https://cloud.example.com',serverName:'Hearfolio',serverVersion:'1.0.0',user:{id:'u1',email:'anna@example.com',name:'Анна'},organizations:[{id:'org-1',name:'Acme',key:'acme',role:'admin'},{id:'org-2',name:'Studio',key:'studio',role:'member'}],createdAt:now,secure:true,needsReauth:false};
  const items:CloudRecording[]=[
    {id:'r1',title:'Планёрка по релизу',recorded_at:iso(now-3600_000),duration_ms:1840_000,status:'transcribing',segments_done:3,segments_total:8,project:{id:'p1',name:'Релиз 2.0',emoji:'🚀'},tags:[{id:'t1',name:'встреча',color:'blue'}],visibility:'organization',transcript_words:0},
    {id:'r2',title:'Интервью с клиентом',recorded_at:iso(now-86400_000),duration_ms:2710_000,status:'transcribed',project:null,tags:[{id:'t2',name:'клиенты',color:'green'},{id:'t3',name:'исследование',color:'purple'}],visibility:'personal',transcript_words:4120,results_count:1,transcript_text:'Добрый день. Расскажите, как вы сейчас ведёте заметки после встреч?\n\nСейчас мы записываем на телефон, а потом кто-то вручную расшифровывает — это занимает полдня.'},
    {id:'r3',title:'Созвон с подрядчиком',recorded_at:iso(now-3*86400_000),duration_ms:600_000,status:'failed',error_message:'Модель не ответила. Повторите.',visibility:'organization',transcript_words:0},
  ];
  libraries.set(libraryKey('conn-demo','org-1'),{items,syncedAt:now,loading:false,error:''});
  projects.set(libraryKey('conn-demo','org-1'),[{id:'p1',name:'Релиз 2.0',emoji:'🚀'},{id:'p2',name:'Исследования',emoji:'🔎',is_private:true}]);
  const uploads:UploadView[]=[
    {clientId:'u-1',connectionId:'conn-demo',organizationId:'org-1',remoteId:null,displayName:say('Запись','Recording')+' 13.11 10.05',recordedAt:now-120_000,durationMs:300_000,size:9_400_000,uploaded:4_230_000,phase:'uploading',error:null,waitingNetwork:false,nextAttemptAt:0,projectId:'p1',visibility:'organization',localRecordId:null,hasTranscript:false,createdAt:now},
    {clientId:'u-2',connectionId:'conn-demo',organizationId:'org-1',remoteId:null,displayName:say('Запись','Recording')+' 13.11 09.40',recordedAt:now-1800_000,durationMs:90_000,size:2_100_000,uploaded:0,phase:'failed',error:'errors.orgServer',waitingNetwork:false,nextAttemptAt:0,projectId:null,visibility:'organization',localRecordId:null,hasTranscript:false,createdAt:now-1},
  ];
  return {connections:[connection],settings:{destination:{connectionId:'conn-demo',organizationId:'org-1',projectId:'p1',visibility:'organization'},retentionDays:30},uploads,cache:{files:4,bytes:57_000_000}};
}
async function call<T>(command:string,args:Record<string,unknown>={}):Promise<T> {
  if(!preview)return nativeCall<T>(command,args);
  if(!previewState)previewState=previewOrg?previewFixture():{connections:[],settings:{destination:null,retentionDays:30},uploads:[],cache:{files:0,bytes:0}};
  const s=previewState,wait=(ms:number)=>new Promise(resolve=>setTimeout(resolve,ms));
  switch(command){
    case 'org_state':return structuredClone(s) as T;
    case 'org_set_destination':s.settings.destination=(args.destination as Destination)||null;return structuredClone(s) as T;
    case 'org_set_retention':s.settings.retentionDays=args.days as number|null;return structuredClone(s) as T;
    case 'org_clear_cache':s.cache={files:0,bytes:0};return structuredClone(s) as T;
    case 'org_projects':await wait(200);return (projects.get(libraryKey(String(args.connectionId),String(args.organizationId)))||[]) as T;
    case 'org_library':case 'org_sync':{await wait(command==='org_sync'?400:0);const library=libraries.get(libraryKey(String(args.connectionId),String(args.organizationId)));return {items:library?.items.map(item=>({...item,transcript_text:undefined}))||[],syncedAt:Date.now()} as T;}
    case 'org_recording':{await wait(150);const item=libraries.get(libraryKey(String(args.connectionId),String(args.organizationId)))?.items.find(row=>row.id===args.recordingId);return (item?{recording:item,results:item.id==='r2'?[{id:'res1',title:say('Краткое содержание','Summary'),status:'done',content:say('Клиент тратит полдня на ручную расшифровку встреч. Главная боль — потеря договорённостей.','The client spends half a day transcribing meetings by hand. The main pain is lost agreements.'),model:'google/gemini-2.5-flash'}]:[],audioCached:false,offline:null}:null) as T;}
    case 'org_connect_start':{await wait(500);const server=String(args.server).trim();if(!server||server.includes(' '))throw new Error('errors.orgUrlInvalid');if(server.startsWith('http://')&&!/localhost|127\.0\.0\.1/.test(server))throw new Error('errors.orgUrlInsecure');
      setTimeout(()=>{previewState=previewFixture();previewEmit('org-connection',{state:'connected',flow:'preview',connection:previewState.connections[0]});},1500);return {state:'preview',redirect:'deepLink'} as T;}
    case 'org_connect_cancel':return undefined as T;
    case 'org_disconnect':s.connections=s.connections.filter(item=>item.id!==args.connectionId);if(s.settings.destination?.connectionId===args.connectionId)s.settings.destination=null;return structuredClone(s) as T;
    case 'org_refresh_connection':await wait(300);return structuredClone(s) as T;
    case 'org_retry_upload':{const upload=s.uploads.find(item=>item.clientId===args.clientId);if(upload){upload.phase='uploading';upload.error=null;}return upload as T;}
    case 'org_cancel_upload':s.uploads=s.uploads.filter(item=>item.clientId!==args.clientId);return undefined as T;
    case 'org_enqueue_record':{const destination=args.destination as Destination,view:UploadView={clientId:crypto.randomUUID(),connectionId:destination.connectionId,organizationId:destination.organizationId,remoteId:null,displayName:say('Новая запись','New recording'),recordedAt:Date.now(),durationMs:3000,size:3_000_000,uploaded:0,phase:'queued',error:null,waitingNetwork:false,nextAttemptAt:0,projectId:destination.projectId,visibility:destination.visibility,localRecordId:null,hasTranscript:false,createdAt:Date.now()};s.uploads.push(view);simulateUpload(view.clientId);return structuredClone(view) as T;}
    case 'org_audio':throw new Error('errors.orgOffline');
    case 'org_retry_recording':{const item=libraries.get(libraryKey(String(args.connectionId),String(args.organizationId)))?.items.find(row=>row.id===args.recordingId);if(item){item.status='queued';item.error_message=null;}return item as T;}
    case 'org_wake':return undefined as T;
    default:throw new Error(`Preview command unavailable: ${command}`);
  }
}
const previewHandlers=new Map<string,Set<(payload:unknown)=>void>>();
function previewEmit(name:string,payload:unknown){previewHandlers.get(name)?.forEach(handler=>handler(payload));}
function listen<T>(name:string,handler:(payload:T)=>void){
  if(!preview){void subscribe<T>(name,handler).catch(()=>{});return;}
  if(!previewHandlers.has(name))previewHandlers.set(name,new Set());previewHandlers.get(name)!.add(handler as (payload:unknown)=>void);
}
function simulateUpload(clientId:string){
  const tick=()=>{const upload=previewState?.uploads.find(item=>item.clientId===clientId);if(!upload)return;upload.phase='uploading';upload.uploaded=Math.min(upload.size,upload.uploaded+upload.size/12);if(upload.uploaded>=upload.size)upload.phase='completing';previewEmit('org-upload',{...upload});if(upload.phase!=='completing')setTimeout(tick,400);};
  setTimeout(tick,600);
}

// ---------------------------------------------------------------- state
export function connections(){return state.connections;}
export function hasOrganizations(){return state.connections.some(connection=>connection.organizations.length>0);}
export function destination(){return state.settings.destination;}
export function isCloudSource(){return source!=='local'&&cloudSource()!==null;}
function cloudSource(){
  if(source==='local')return null;
  const [connectionId,organizationId]=source.split(':');const found=organization(connectionId,organizationId);
  return found.organization?{connectionId,organizationId}:null;
}
export function cloudOpen(){return open;}
async function refreshState(){
  try{state=await call<OrgState>('org_state');loaded=true;}catch{/* Keep the last known state. */}
  if(source!=='local'&&!cloudSource())setSource('local');
}
function setSource(next:string){source=next;try{localStorage.setItem('hearfolio-source',next);}catch{/* Per-device convenience only. */}}

export async function initOrg(context:Context){
  ctx=context;
  await refreshState();
  listen<UploadView>('org-upload',upload=>{
    const index=state.uploads.findIndex(item=>item.clientId===upload.clientId),previous=index>=0?state.uploads[index]:null;
    if(index>=0)state.uploads[index]=upload;else state.uploads.push(upload);
    if(open?.clientId===upload.clientId&&(upload.remoteId&&!open.recordingId||upload.phase==='done'&&previous?.phase!=='done')){open.recordingId=upload.remoteId;void loadDetail(true);}
    if(previous&&previous.phase===upload.phase&&previous.waitingNetwork===upload.waitingNetwork&&previous.error===upload.error)patchUpload(upload);else rerender();
  });
  listen<{connectionId:string;organizationId:string}>('org-library',payload=>{void loadLibrary(payload.connectionId,payload.organizationId,false);if(open&&open.connectionId===payload.connectionId&&open.organizationId===payload.organizationId)void loadDetail(false);});
  listen<{state:string;flow?:string;connection?:Connection;error?:string;connectionId?:string}>('org-connection',payload=>{void connectionEvent(payload);});
  listen<{recordingId:string;current:number;total:number;done?:boolean}>('org-audio',payload=>{if(audio&&audio.key===payload.recordingId&&payload.total){audio.progress=Math.floor(payload.current/payload.total*100);const label=document.querySelector('[data-org-audio-progress]');if(label)label.textContent=`${audio.progress}%`;}});
  const wake=()=>{if(document.hidden)return;void call('org_wake').catch(()=>{});void refreshVisible();};
  document.addEventListener('visibilitychange',wake);window.addEventListener('focus',wake);window.addEventListener('online',wake);
  // While open, the visible organization stays fresh; a running transcription refreshes faster.
  setInterval(()=>{if(!document.hidden)void refreshVisible();},60_000);
  setInterval(()=>{if(!document.hidden&&open&&detail&&['transcribing','queued'].includes(detail.recording.status||''))void loadDetail(true);},10_000);
  const current=cloudSource();if(current)void loadLibrary(current.connectionId,current.organizationId,true);
}
let renderQueued=false;
function rerender(){
  if(renderQueued)return;renderQueued=true;
  requestAnimationFrame(()=>{renderQueued=false;const focused=document.activeElement;if(focused instanceof HTMLInputElement&&focused.type!=='range'||focused instanceof HTMLTextAreaElement)return;ctx.render();});
}
function patchUpload(upload:UploadView){
  const status=uploadStatus(upload,say);
  document.querySelectorAll<HTMLElement>(`[data-upload-status="${CSS.escape(upload.clientId)}"]`).forEach(element=>{if(element.textContent!==status.label)element.textContent=status.label;});
  document.querySelectorAll<HTMLElement>(`[data-upload-bar="${CSS.escape(upload.clientId)}"]`).forEach(element=>element.style.width=`${status.percent??0}%`);
}
async function refreshVisible(){
  const current=open||cloudSource();
  if(current)await loadLibrary(current.connectionId,current.organizationId,true);
}
async function loadLibrary(connectionId:string,organizationId:string,sync:boolean){
  const key=libraryKey(connectionId,organizationId),library=libraries.get(key)||{items:[],syncedAt:null,loading:false,error:''};
  libraries.set(key,library);
  if(!library.syncedAt&&!library.items.length){try{const stored=await call<{items:CloudRecording[];syncedAt:number|null}>('org_library',{connectionId,organizationId});library.items=stored.items;library.syncedAt=stored.syncedAt;}catch{/* Sync below reports errors. */}}
  if(sync){
    if(library.loading)return;library.loading=true;rerender();
    try{const synced=await call<{items:CloudRecording[];syncedAt:number|null}>('org_sync',{connectionId,organizationId});library.items=synced.items;library.syncedAt=synced.syncedAt;library.error='';}
    catch(error){library.error=orgErrorText(error);if(String(error).includes('orgUnauthorized'))await refreshState();}
    finally{library.loading=false;}
  }else{try{const stored=await call<{items:CloudRecording[];syncedAt:number|null}>('org_library',{connectionId,organizationId});library.items=stored.items;}catch{/* Keep what is shown. */}}
  rerender();
}

// ---------------------------------------------------------------- list
export function sourceTabsHtml():string {
  if(!hasOrganizations())return '';
  const tabs=[{id:'local',label:say('На устройстве','On device'),count:0}];
  for(const connection of state.connections)for(const org of connection.organizations){
    const id=libraryKey(connection.id,org.id),pending=state.uploads.filter(upload=>upload.connectionId===connection.id&&upload.organizationId===org.id&&upload.phase!=='done').length;
    tabs.push({id,label:org.name,count:pending});
  }
  return `<div class="source-tabs" role="tablist" aria-label="${say('Где хранятся записи','Where recordings are')}">${tabs.map(tab=>`<button type="button" role="tab" class="source-tab" data-org-source="${esc(tab.id)}" aria-selected="${source===tab.id}">${tab.id==='local'?icon('file'):icon('cloud')}<span>${esc(tab.label)}</span>${tab.count?`<small class="source-count">${tab.count}</small>`:''}</button>`).join('')}</div>`;
}
function statusChip(status:Status,clientId?:string):string {
  return `<span class="record-state org-state tone-${status.tone}" ${clientId?`data-upload-status="${esc(clientId)}"`:''}>${status.tone==='progress'?'<i aria-hidden="true"></i>':''}${esc(status.label)}</span>`;
}
function rows():Row[] {
  const current=cloudSource();if(!current)return [];
  const library=libraries.get(libraryKey(current.connectionId,current.organizationId));
  return mergeRows(library?.items||[],state.uploads,current.connectionId,current.organizationId,say);
}
export function cloudListHtml():string {
  const current=cloudSource();if(!current)return '';
  const library=libraries.get(libraryKey(current.connectionId,current.organizationId));
  const query=ctx.searchText().toLocaleLowerCase(),all=rows(),list=query?all.filter(row=>row.title.toLocaleLowerCase().includes(query)):all;
  const {connection}=organization(current.connectionId,current.organizationId);
  const reauth=connection?.needsReauth?`<div class="org-inline-alert" role="alert">${icon('alert')}<span>${say('Доступ к организации истёк.','Access to the organization expired.')}</span><button type="button" class="text-button" data-org-reauth="${esc(connection.id)}">${say('Войти','Sign in')}</button></div>`:'';
  const offline=library?.error?`<div class="org-inline-note">${esc(library.error)}</div>`:'';
  if(!list.length){
    if(library?.loading||!library)return `${reauth}<div class="list-empty"><span class="spinner"></span>${say('Загружаем записи…','Loading recordings…')}</div>`;
    return `${reauth}${offline}<div class="list-empty first-run"><strong>${query?say('Записи не найдены','No recordings found'):say('Здесь пока пусто','Nothing here yet')}</strong>${query?'':`<span>${say('Выберите эту организацию в «Куда записываем» — новые записи загрузятся сюда.','Choose this organization under “Record to” — new recordings will upload here.')}</span>`}</div>`;
  }
  const selected=open?.recordingId||open?.clientId;
  return `${reauth}${offline}${list.map(row=>{
    const id=row.item?.id||'',client=row.upload?.clientId||'',meta=[dateLabel(row.recordedAt),timeLabel(row.durationMs),row.item?.project?`${row.item.project.emoji||''} ${row.item.project.name}`.trim():''].filter(Boolean);
    const bar=row.status.tone==='progress'&&row.upload?`<span class="org-row-bar" aria-hidden="true"><i data-upload-bar="${esc(client)}" style="width:${row.status.percent??0}%"></i></span>`:'';
    return `<div class="record-row org-row ${selected&&(selected===id||selected===client)?'selected':''}"><button class="record-link" data-org-open="${esc(id)}" data-org-client="${esc(client)}"><span class="record-title">${esc(row.title)}</span><span class="record-detail">${meta.map(part=>`<span>${esc(part)}</span>`).join('')}${row.item?.visibility==='personal'?`<span>${say('Личная','Personal')}</span>`:''}</span>${bar}</button>${statusChip(row.status,row.upload&&row.upload.phase!=='done'?client:undefined)}</div>`;
  }).join('')}${library?.loading?`<div class="org-list-sync"><span class="spinner"></span>${say('Обновляем…','Refreshing…')}</div>`:''}`;
}

// ---------------------------------------------------------------- detail
function openUpload(){return open?.clientId?state.uploads.find(upload=>upload.clientId===open!.clientId)||null:null;}
async function openRecording(connectionId:string,organizationId:string,recordingId:string|null,clientId:string|null){
  if(!recordingId&&clientId)recordingId=state.uploads.find(upload=>upload.clientId===clientId)?.remoteId||null;
  open={connectionId,organizationId,recordingId,clientId};detailError='';detailTab='transcript';audio=null;
  // The list row is shown at once; the transcript and summaries follow.
  const row=recordingId?libraries.get(libraryKey(connectionId,organizationId))?.items.find(item=>item.id===recordingId):null;
  detail=row?{recording:row,results:null,audioCached:false,offline:null}:null;
  ctx.openDetail();
  if(recordingId)await loadDetail(true);
}
async function loadDetail(refresh:boolean){
  if(!open)return;
  if(!open.recordingId){const upload=openUpload();if(upload?.remoteId)open.recordingId=upload.remoteId;else{rerender();return;}}
  const target=open;detailLoading=!detail;detailFetching=true;rerender();
  try{
    const value=await call<Detail|null>('org_recording',{connectionId:target.connectionId,organizationId:target.organizationId,recordingId:target.recordingId,refresh});
    if(open!==target)return;
    if(!value){if(!openUpload()){detailError=say('Запись удалена или больше недоступна.','The recording was deleted or is no longer available.');detail=null;}}
    else{detail={...value,results:value.results||null};detailError='';}
  }catch(error){if(open===target)detailError=orgErrorText(error);}
  finally{detailFetching=false;if(open===target){detailLoading=false;rerender();}}
}
export function closeDetail(){open=null;detail=null;audio=null;void releaseNative();}
export function detailTitle():string {return detail?.recording.title||openUpload()?.displayName||say('Запись','Recording');}
function resultsHtml(results:Result[]|null):string {
  if(!results&&detailFetching)return `<div class="list-empty"><span class="spinner"></span>${say('Загружаем выжимки…','Loading summaries…')}</div>`;
  if(!results)return `<div class="org-empty">${say('Выжимки загрузятся, когда появится сеть.','Summaries will load when you are online.')}</div>`;
  if(!results.length)return `<div class="org-empty">${say('Выжимок пока нет. Их создают в веб-версии или автоматически по правилам организации.','No summaries yet. They are created on the web or automatically by organization rules.')}</div>`;
  return results.map(result=>{
    const running=result.status==='queued'||result.status==='running',failed=result.status==='failed';
    return `<article class="org-result"><header><strong>${esc(result.title||say('Выжимка','Summary'))}</strong>${running?`<span class="record-state org-state tone-progress"><i aria-hidden="true"></i>${say('Готовится','Generating')}</span>`:failed?`<span class="record-state org-state tone-error">${say('Ошибка','Failed')}</span>`:''}</header>${failed?`<p class="org-muted">${esc(result.error_message||'')}</p>`:running?'':`<div class="org-result-text">${esc(result.content||'')}</div>`}${result.model?`<small class="org-muted">${esc(result.model)}</small>`:''}</article>`;
  }).join('');
}
function audioHtml():string {
  const recording=detail?.recording;if(!recording||!['queued','transcribing','transcribed','failed'].includes(recording.status||''))return '';
  if(audio?.src&&!ctx.android())return `<div class="audio-bar"><audio id="org-audio" controls preload="metadata" src="${esc(audio.src)}"></audio></div>`;
  if(ctx.android()&&audio?.src)return `<div class="audio-bar org-native-audio"><button type="button" class="play-button" id="org-native-play" aria-label="${say('Воспроизвести','Play')}">${icon(nativePlaying?'pause':'play')}</button><span class="org-muted" id="org-native-time">${timeLabel(nativePosition*1000)} / ${timeLabel(recording.duration_ms||0)}</span></div>`;
  if(audio?.loading)return `<div class="audio-bar org-audio-loading"><span class="spinner"></span><span>${say('Загружаем аудио','Loading audio')} <span data-org-audio-progress>${audio.progress?`${audio.progress}%`:''}</span></span></div>`;
  return `<div class="audio-bar org-audio-actions"><button type="button" class="secondary" id="org-play">${icon('play')}<span>${detail?.audioCached?say('Слушать','Listen'):say('Слушать онлайн','Stream audio')}</span></button>${audio?.error?`<span class="org-muted" role="alert">${esc(audio.error)}</span>`:detail?.audioCached?`<span class="org-muted">${say('Сохранено на устройстве','Saved on this device')}</span>`:''}</div>`;
}
export function cloudDetailHtml():string {
  if(!open)return '';
  const upload=openUpload(),recording=detail?.recording,pendingUpload=upload&&upload.phase!=='done'?upload:null;
  const {organization:org}=organization(open.connectionId,open.organizationId);
  const status=pendingUpload?uploadStatus(pendingUpload,say):recording?recordingStatus(recording,say):null;
  const recordedAt=recording?Date.parse(recording.recorded_at||''):upload?.recordedAt||0;
  const meta=[dateLabel(recordedAt),timeLabel(recording?.duration_ms??upload?.durationMs??null),org?.name||'',recording?.project?`${recording.project.emoji||''} ${recording.project.name}`.trim():'',(recording?.visibility||upload?.visibility)==='personal'?say('Личная','Personal'):say('Общая','Shared')].filter(Boolean);
  const tags=recording?.tags?.length?`<div class="org-tags">${recording.tags.map(tag=>`<span class="org-tag tag-${esc(tag.color)}">${esc(tag.name)}</span>`).join('')}</div>`:'';
  let banner='';
  if(status&&status.tone!=='done'){
    const actions=pendingUpload?.phase==='failed'
      ?`${status.action==='retry-upload'?`<button type="button" class="primary" data-org-retry-upload="${esc(pendingUpload.clientId)}">${icon('history')}<span>${say('Повторить','Retry')}</span></button>`:''}<button type="button" class="secondary" data-org-keep-local="${esc(pendingUpload.clientId)}">${say(pendingUpload.localRecordId?'Отменить копирование':'Сохранить на устройстве',pendingUpload.localRecordId?'Cancel copy':'Keep on device')}</button>`
      :pendingUpload?`<button type="button" class="text-button danger" data-org-cancel-upload="${esc(pendingUpload.clientId)}">${say('Отменить','Cancel')}</button>`
      :status.action==='retry-transcription'?`<button type="button" class="primary" id="org-retry-transcription">${icon('history')}<span>${say('Повторить','Retry')}</span></button>`:'';
    const explanation=pendingUpload?.phase==='failed'?orgErrorText(pendingUpload.error||''):pendingUpload?.waitingNetwork?say('Загрузка продолжится, как только появится сеть. Аудио в безопасности на устройстве.','The upload resumes as soon as you are online. The audio is safe on this device.'):pendingUpload?say('Можно продолжать работу — загрузка идёт в фоне.','Keep working — the upload continues in the background.'):recording?.status==='failed'?(recording.error_message||''):recording?.status==='transcribing'||recording?.status==='queued'?say('Сервер расшифровывает запись. Текст появится здесь.','The server is transcribing. Text will appear here.'):'';
    banner=`<div class="org-status-card tone-${status.tone}" role="status"><div class="org-status-line"><strong ${pendingUpload?`data-upload-status="${esc(pendingUpload.clientId)}"`:''}>${esc(status.label)}</strong>${status.percent!==null?`<span class="org-status-bar"><i ${pendingUpload?`data-upload-bar="${esc(pendingUpload.clientId)}"`:''} style="width:${status.percent}%"></i></span>`:''}</div>${explanation?`<p>${esc(explanation)}</p>`:''}${actions?`<div class="org-status-actions">${actions}</div>`:''}</div>`;
  }
  const transcript=recording?.transcript_text||'';
  const results=detail?.results||null;
  const tabs=recording?`<div class="output-tabs"><div class="output-tab-buttons" role="tablist"><button role="tab" data-org-tab="transcript" aria-selected="${detailTab==='transcript'}">${say('Текст','Text')}</button><button role="tab" data-org-tab="results" aria-selected="${detailTab==='results'}">${say('Выжимки','Summaries')}${results?.length?` <small>${results.length}</small>`:''}</button></div>${detailTab==='transcript'&&transcript?`<div class="output-tools"><button type="button" class="ghost-button" id="org-copy">${icon('copy')}<span>${say('Копировать','Copy')}</span></button></div>`:''}</div>`:'';
  const body=detailLoading&&!recording?`<div class="list-empty"><span class="spinner"></span>${say('Загружаем запись…','Loading recording…')}</div>`
    :detailError&&!recording?`<div class="list-empty">${esc(detailError)}<button class="text-button" id="org-detail-retry">${say('Повторить','Retry')}</button></div>`
    :!recording?`<div class="org-empty">${say('Текст появится после загрузки и расшифровки на сервере.','Text appears after the upload and server transcription.')}</div>`
    :detailTab==='results'?resultsHtml(results)
    :!transcript&&detailFetching&&recording.status==='transcribed'?`<div class="list-empty"><span class="spinner"></span>${say('Загружаем текст…','Loading text…')}</div>`
    :transcript?`<article class="transcript" lang="${esc(recording.language||currentLanguage())}">${esc(transcript)}</article>`
    :`<div class="org-empty">${recording.status==='failed'?say('Расшифровка не получилась.','Transcription failed.'):say('Текст появится после расшифровки.','Text will appear after transcription.')}</div>`;
  const offline=detail?.offline?`<div class="org-inline-note">${esc(orgErrorText(detail.offline))}</div>`:'';
  return `<section class="workspace org-detail"><header class="record-header"><div class="record-heading"><div class="record-name-row"><h1>${esc(detailTitle())}</h1></div><div class="record-meta">${meta.map(part=>`<span>${esc(part)}</span>`).join('')}</div>${tags}</div></header>${offline}${banner}${audioHtml()}${tabs}<div class="transcript-scroll">${body}</div></section>`;
}

// ---------------------------------------------------------------- audio
let nativePlaying=false,nativePosition=0,nativeTimer:ReturnType<typeof setInterval>|null=null;
async function releaseNative(){if(nativeTimer)clearInterval(nativeTimer);nativeTimer=null;if(nativePlaying||audio?.src){nativePlaying=false;if(!preview&&ctx?.android())await nativeCall('release_native_playback').catch(()=>{});}}
async function playAudio(){
  if(!open?.recordingId||audio?.loading)return;
  const target=open,recordingId=open.recordingId;
  audio={key:recordingId,src:'',loading:true,error:'',progress:0};rerender();
  try{
    if(ctx.android()){
      const path=await call<string>('org_cache_audio',{connectionId:target.connectionId,organizationId:target.organizationId,recordingId});
      await call('org_prepare_native_playback',{path});await nativeCall('play_native_playback');
      if(open!==target)return;
      audio={key:recordingId,src:path,loading:false,error:'',progress:100};nativePlaying=true;
      nativeTimer=setInterval(async()=>{try{const player=await nativeCall<{currentTime:number;playing:boolean}>('native_playback_state');nativePosition=player.currentTime;nativePlaying=player.playing;const label=document.querySelector('#org-native-time');if(label)label.textContent=`${timeLabel(nativePosition*1000)} / ${timeLabel(detail?.recording.duration_ms||0)}`;}catch{/* Player released. */}},500);
    }else{
      const value=await call<{path:string|null;url:string|null}>('org_audio',{connectionId:target.connectionId,organizationId:target.organizationId,recordingId});
      if(open!==target)return;
      audio={key:recordingId,src:value.path?convertFileSrc(value.path):value.url||'',loading:false,error:'',progress:100};
      // Streamed now, cached in the background for offline listening.
      if(!value.path)void call('org_cache_audio',{connectionId:target.connectionId,organizationId:target.organizationId,recordingId}).then(()=>{if(detail&&open===target)detail.audioCached=true;}).catch(()=>{});
    }
  }catch(error){if(open===target)audio={key:recordingId,src:'',loading:false,error:orgErrorText(error),progress:0};}
  rerender();
  requestAnimationFrame(()=>{void document.querySelector<HTMLAudioElement>('#org-audio')?.play().catch(()=>{});});
}

// ---------------------------------------------------------------- destination
export function destinationLabel(value=state.settings.destination):string {
  if(!value)return say('На устройстве','On this device');
  const {organization:org}=organization(value.connectionId,value.organizationId);
  if(!org)return say('На устройстве','On this device');
  const project=projects.get(libraryKey(value.connectionId,value.organizationId))?.find(item=>item.id===value.projectId);
  return [org.name,project?`${project.emoji||''} ${project.name}`.trim():'',value.visibility==='personal'?say('личная','personal'):''].filter(Boolean).join(' · ');
}
export function destinationChipHtml(compact=false):string {
  if(!hasOrganizations())return '';
  const cloud=Boolean(state.settings.destination);
  return `<button type="button" class="destination-chip ${cloud?'is-cloud':''} ${compact?'compact':''}" data-org-destination aria-haspopup="dialog" ${ctx.busy()?'disabled':''}><span class="destination-label">${say('Куда','To')}</span>${icon(cloud?'cloud':'file')}<span class="destination-value">${esc(destinationLabel())}</span>${icon('chevron')}</button>`;
}
async function ensureProjects(connectionId:string,organizationId:string){
  const key=libraryKey(connectionId,organizationId);if(projects.has(key))return projects.get(key)!;
  try{const list=await call<Project[]>('org_projects',{connectionId,organizationId});projects.set(key,list);return list;}catch{return [];}
}
/** The destination sheet. `copy`: choose an organization for an existing local recording. */
function chooseDestination(copy:boolean):Promise<Destination|null|undefined> {
  return new Promise(resolve=>{
    const current=state.settings.destination;
    let choice:Destination|null=copy?(current||firstOrganization()):current;
    const dialog=document.createElement('dialog');dialog.className='record-actions-dialog org-destination-dialog';
    const paint=async()=>{
      const list=choice?await ensureProjects(choice.connectionId,choice.organizationId):[];
      const option=(id:string,label:string,detailText:string,selected:boolean,glyph:string)=>`<button type="button" class="org-option" data-choice="${esc(id)}" aria-pressed="${selected}">${icon(glyph)}<span><strong>${esc(label)}</strong>${detailText?`<small>${esc(detailText)}</small>`:''}</span>${selected?icon('check'):''}</button>`;
      const orgOptions=state.connections.flatMap(connection=>connection.organizations.map(org=>option(libraryKey(connection.id,org.id),org.name,host(connection.server)+(connection.needsReauth?` · ${say('нужно войти','sign in required')}`:''),choice?.connectionId===connection.id&&choice?.organizationId===org.id,'cloud'))).join('');
      dialog.innerHTML=`<div class="record-actions-heading"><h2>${copy?say('Копировать в организацию','Copy to organization'):say('Куда записываем','Record to')}</h2><button type="button" class="icon-button" data-close aria-label="${say('Закрыть','Close')}">${icon('close')}</button></div><div class="record-actions-group org-options">${copy?'':option('local',say('На этом устройстве','On this device'),say('Расшифровка выбранной моделью','Transcribed with your chosen model'),!choice,'file')}${orgOptions}</div>${choice?`<div class="org-destination-extra"><label class="field"><span>${say('Проект','Project')}</span><select data-project><option value="">${say('Без проекта','No project')}</option>${list.map(project=>`<option value="${esc(project.id)}" ${choice!.projectId===project.id?'selected':''}>${esc(`${project.emoji||''} ${project.name}`.trim())}${project.is_private?' 🔒':''}</option>`).join('')}</select></label><div class="segmented org-visibility" role="group" aria-label="${say('Кто видит','Who can see')}"><button type="button" data-visibility="organization" aria-pressed="${choice.visibility==='organization'}">${say('Общая','Shared')}</button><button type="button" data-visibility="personal" aria-pressed="${choice.visibility==='personal'}">${say('Личная','Personal')}</button></div><small class="org-muted">${choice.visibility==='personal'?say('Видите только вы.','Only you can see it.'):say('Видят участники организации (или проекта).','Visible to organization (or project) members.')}</small></div>`:''}<div class="org-dialog-actions"><button type="button" class="primary" data-done>${copy?say('Копировать','Copy'):say('Готово','Done')}</button></div>`;
      dialog.querySelector('[data-close]')?.addEventListener('click',()=>{settled=true;dialog.close();resolve(undefined);});
      dialog.querySelectorAll<HTMLElement>('[data-choice]').forEach(button=>button.onclick=()=>{const id=button.dataset.choice!;if(id==='local')choice=null;else{const [connectionId,organizationId]=id.split(':');choice={connectionId,organizationId,projectId:null,visibility:choice?.visibility||'organization'};}void paint();});
      dialog.querySelector<HTMLSelectElement>('[data-project]')?.addEventListener('change',event=>{if(choice)choice.projectId=(event.target as HTMLSelectElement).value||null;});
      dialog.querySelectorAll<HTMLElement>('[data-visibility]').forEach(button=>button.onclick=()=>{if(choice){choice.visibility=button.dataset.visibility as Destination['visibility'];void paint();}});
      dialog.querySelector('[data-done]')?.addEventListener('click',()=>{settled=true;dialog.close();resolve(choice);});
    };
    let settled=false;
    dialog.addEventListener('click',event=>{if(event.target===dialog){const rect=dialog.getBoundingClientRect();if(event.clientX<rect.left||event.clientX>rect.right||event.clientY<rect.top||event.clientY>rect.bottom){settled=true;dialog.close();resolve(undefined);}}});
    dialog.addEventListener('close',()=>{dialog.remove();if(!settled)resolve(undefined);});
    document.body.append(dialog);void paint().then(()=>{dialog.showModal();dialog.querySelector<HTMLElement>('[aria-pressed="true"]')?.focus();});
  });
}
function firstOrganization():Destination|null {
  const connection=state.connections.find(item=>item.organizations.length);
  return connection?{connectionId:connection.id,organizationId:connection.organizations[0].id,projectId:null,visibility:'organization'}:null;
}
async function pickDestination(){
  const chosen=await chooseDestination(false);if(chosen===undefined)return;
  try{state=await call<OrgState>('org_set_destination',{destination:chosen});}catch(error){ctx.notice(orgErrorText(error),true);}
  ctx.render();
}

/**
 * After a new recording or file entered the local archive: if the destination
 * is an organization, hand it to the upload queue and show its cloud screen.
 * Returns true when the recording moved to an organization.
 */
export async function afterImport(entry:Entry,kind:'recording'|'import'):Promise<boolean> {
  const target=state.settings.destination;if(!target||!organization(target.connectionId,target.organizationId).organization)return false;
  try{
    const upload=await call<UploadView>('org_enqueue_record',{id:entry.id,destination:target,moveLocal:true,title:null,source:kind});
    state.uploads=[...state.uploads.filter(item=>item.clientId!==upload.clientId),upload];
    setSource(libraryKey(target.connectionId,target.organizationId));
    await openRecording(target.connectionId,target.organizationId,null,upload.clientId);
    return true;
  }catch(error){ctx.notice(`${say('Запись сохранена на устройстве, но не поставлена в загрузку: ','Saved on this device but not queued for upload: ')}${orgErrorText(error)}`,true);return false;}
}
export async function copyToOrganization(entry:Entry,displayName:string){
  if(!hasOrganizations())return;
  const chosen=await chooseDestination(true);if(!chosen)return;
  try{
    const upload=await call<UploadView>('org_enqueue_record',{id:entry.id,destination:chosen,moveLocal:false,title:displayName,source:'import'});
    state.uploads=[...state.uploads.filter(item=>item.clientId!==upload.clientId),upload];
    const {organization:org}=organization(chosen.connectionId,chosen.organizationId);
    ctx.notice(upload.hasTranscript?say(`Копируем в «${org?.name}» вместе с текстом`,`Copying to “${org?.name}” with the transcript`):say(`Копируем в «${org?.name}»`,`Copying to “${org?.name}”`));
    rerender();
  }catch(error){ctx.notice(orgErrorText(error),true);}
}

// ---------------------------------------------------------------- settings
export function orgSettingsHtml():string {
  const busyFlow=flow.phase!=='idle';
  const flowText=flow.phase==='checking'?say('Проверяем сервер…','Checking the server…'):flow.phase==='browser'?say('Подтвердите вход в открывшемся браузере, затем вернитесь сюда.','Confirm sign-in in the browser, then come back here.'):flow.phase==='exchanging'?say('Завершаем вход…','Finishing sign-in…'):'';
  const cards=state.connections.map(connection=>`<article class="org-connection"><header><div><strong>${esc(connection.serverName||'Hearfolio')}</strong><small>${esc(host(connection.server))} · ${esc(connection.user.email||connection.user.name)}</small></div>${connection.needsReauth?`<span class="badge warning">${say('Нужно войти','Sign in required')}</span>`:connection.secure?'':`<span class="badge" title="${say('Системное хранилище ключей недоступно: ключ лежит в файле, доступном только вашей учётной записи.','System keychain unavailable: the key is stored in a file readable only by your account.')}">${say('Ключ в файле','Key in file')}</span>`}</header><ul class="org-list">${connection.organizations.map(org=>`<li>${icon('cloud')}<span>${esc(org.name)}</span><small>${esc(org.role==='owner'?say('владелец','owner'):org.role==='admin'?say('админ','admin'):say('участник','member'))}</small></li>`).join('')||`<li class="org-muted">${say('Нет организаций. Создайте или примите приглашение в веб-версии.','No organizations. Create one or accept an invite on the web.')}</li>`}</ul><div class="org-connection-actions">${connection.needsReauth?`<button type="button" class="primary" data-org-reauth="${esc(connection.id)}" ${busyFlow?'disabled':''}>${say('Войти снова','Sign in again')}</button>`:`<button type="button" class="secondary" data-org-refresh="${esc(connection.id)}">${icon('history')}<span>${say('Обновить','Refresh')}</span></button>`}<button type="button" class="text-button danger" data-org-disconnect="${esc(connection.id)}">${say('Отключить','Disconnect')}</button></div></article>`).join('');
  const retention=state.settings.retentionDays;
  return `<section class="settings-section"><div class="section-heading"><h2>${say('Организации','Organizations')}</h2></div><p>${say('Подключите сервер Hearfolio Cloud своей команды. Записи загружаются туда, расшифровываются на сервере и появляются на всех ваших устройствах.','Connect your team’s Hearfolio Cloud server. Recordings upload there, are transcribed on the server and appear on all your devices.')}</p>${cards}<form id="org-connect-form" class="org-connect"><label class="field"><span>${say('Адрес сервера','Server address')}</span><input name="server" type="text" inputmode="url" autocomplete="url" autocapitalize="off" spellcheck="false" placeholder="cloud.example.com" value="${esc(serverDraft)}" ${busyFlow?'disabled':''} required></label><div class="org-connect-row">${busyFlow?`<span class="org-flow"><span class="spinner"></span>${esc(flowText)}</span><button type="button" class="secondary" id="org-connect-cancel">${say('Отмена','Cancel')}</button>`:`<button type="submit" class="primary">${icon('plus')}<span>${state.connections.length?say('Подключить ещё','Connect another'):say('Подключить','Connect')}</span></button>`}</div>${flow.error?`<p class="settings-error" role="alert">${esc(flow.error)}</p>`:''}</form></section><section class="settings-section"><h2>${say('Хранить аудио на устройстве','Keep audio on device')}</h2><p>${say('Аудио из облака сохраняется после загрузки и прослушивания и удаляется, если его долго не открывали. Текст и выжимки остаются.','Cloud audio is kept after upload or playback and removed when unused for a while. Text and summaries stay.')}</p><label class="field"><span>${say('Срок','Keep for')}</span><select id="org-retention">${RETENTION_CHOICES.map(days=>`<option value="${days??'forever'}" ${retention===days?'selected':''}>${days===null?say('Всегда','Forever'):say(`${days} дней`,`${days} days`)}</option>`).join('')}</select></label><div class="org-cache-row"><span>${say('Занято','Used')}: ${mb(state.cache.bytes)}</span><button type="button" class="text-button" id="org-clear-cache" ${state.cache.files?'':'disabled'}>${say('Очистить','Clear')}</button></div></section>`;
}
async function connectionEvent(payload:{state:string;flow?:string;connection?:Connection;error?:string}){
  if(payload.state==='reauth'){await refreshState();rerender();return;}
  if(payload.flow&&flow.state&&payload.flow!==flow.state&&payload.flow!=='preview')return;
  if(payload.state==='exchanging'){flow={...flow,phase:'exchanging'};rerender();return;}
  if(payload.state==='connected'){
    flow={phase:'idle',state:'',error:''};serverDraft='';await refreshState();
    const org=payload.connection?.organizations[0];
    ctx.notice(say(`Подключено: ${payload.connection?.user.email||''}`,`Connected: ${payload.connection?.user.email||''}`));
    if(org&&!state.settings.destination)void ensureProjects(payload.connection!.id,org.id);
  }else if(payload.state==='cancelled'){flow={phase:'idle',state:'',error:say('Вход отменён.','Sign-in was cancelled.')};}
  else if(payload.state==='failed'){flow={phase:'idle',state:'',error:orgErrorText(payload.error||'')};}
  ctx.render();
}
async function connect(server:string,replaces?:string){
  if(flow.phase!=='idle')return;
  serverDraft=server;flow={phase:'checking',state:'',error:''};ctx.render();
  try{const started=await call<{state:string;redirect:string}>('org_connect_start',{server,replaces:replaces||null});if(flow.phase==='checking')flow={phase:'browser',state:started.state,error:''};}
  catch(error){flow={phase:'idle',state:'',error:orgErrorText(error)};}
  ctx.render();
}

// ---------------------------------------------------------------- binding
export function bindOrg(){
  document.querySelectorAll<HTMLElement>('[data-org-source]').forEach(button=>button.onclick=()=>{
    const next=button.dataset.orgSource!;if(next===source)return;setSource(next);
    const current=cloudSource();if(current)void loadLibrary(current.connectionId,current.organizationId,true);ctx.render();
  });
  document.querySelectorAll<HTMLElement>('[data-org-open]').forEach(button=>button.onclick=()=>{const current=cloudSource();if(current)void openRecording(current.connectionId,current.organizationId,button.dataset.orgOpen||null,button.dataset.orgClient||null);});
  document.querySelectorAll<HTMLElement>('[data-org-destination]').forEach(button=>button.onclick=()=>{void pickDestination();});
  document.querySelectorAll<HTMLElement>('[data-org-retry-upload]').forEach(button=>button.onclick=async()=>{try{await call('org_retry_upload',{clientId:button.dataset.orgRetryUpload});}catch(error){ctx.notice(orgErrorText(error),true);}await refreshState();rerender();});
  document.querySelectorAll<HTMLElement>('[data-org-cancel-upload]').forEach(button=>button.onclick=async()=>{
    if(!await ctx.confirm(say('Отменить загрузку?','Cancel upload?'),say('Аудио этой записи будет удалено с устройства и не попадёт в организацию.','The audio of this recording will be deleted from this device and will not reach the organization.'),say('Удалить запись','Delete recording')))return;
    try{await call('org_cancel_upload',{clientId:button.dataset.orgCancelUpload});state.uploads=state.uploads.filter(item=>item.clientId!==button.dataset.orgCancelUpload);ctx.closeDetail();}catch(error){ctx.notice(orgErrorText(error),true);}
  });
  document.querySelectorAll<HTMLElement>('[data-org-keep-local]').forEach(button=>button.onclick=async()=>{
    try{const entry=await call<Entry>('org_keep_local',{clientId:button.dataset.orgKeepLocal});state.uploads=state.uploads.filter(item=>item.clientId!==button.dataset.orgKeepLocal);setSource('local');open=null;detail=null;ctx.openLocal(entry);}catch(error){ctx.notice(orgErrorText(error),true);}
  });
  document.querySelector('#org-retry-transcription')?.addEventListener('click',async()=>{if(!open?.recordingId)return;try{await call('org_retry_recording',{connectionId:open.connectionId,organizationId:open.organizationId,recordingId:open.recordingId});await loadDetail(true);}catch(error){ctx.notice(orgErrorText(error),true);}});
  document.querySelector('#org-detail-retry')?.addEventListener('click',()=>{void loadDetail(true);});
  document.querySelector('#org-play')?.addEventListener('click',()=>{void playAudio();});
  document.querySelector('#org-native-play')?.addEventListener('click',async()=>{try{if(nativePlaying)await nativeCall('pause_native_playback');else await nativeCall('play_native_playback');nativePlaying=!nativePlaying;rerender();}catch(error){ctx.notice(errorText(error),true);}});
  document.querySelectorAll<HTMLElement>('[data-org-tab]').forEach(button=>button.onclick=()=>{detailTab=button.dataset.orgTab as typeof detailTab;ctx.render();});
  document.querySelector('#org-copy')?.addEventListener('click',async()=>{try{await navigator.clipboard.writeText(detail?.recording.transcript_text||'');ctx.notice(say('Скопировано','Copied'));}catch{ctx.notice(say('Не удалось скопировать','Could not copy'),true);}});
  const form=document.querySelector<HTMLFormElement>('#org-connect-form');
  form?.addEventListener('input',()=>{serverDraft=String(new FormData(form).get('server')||'');});
  form?.addEventListener('submit',event=>{event.preventDefault();void connect(String(new FormData(form).get('server')||''));});
  document.querySelector('#org-connect-cancel')?.addEventListener('click',()=>{if(flow.state)void call('org_connect_cancel',{state:flow.state}).catch(()=>{});flow={phase:'idle',state:'',error:''};ctx.render();});
  document.querySelectorAll<HTMLElement>('[data-org-reauth]').forEach(button=>button.onclick=()=>{const connection=state.connections.find(item=>item.id===button.dataset.orgReauth);if(connection)void connect(connection.server,connection.id);});
  document.querySelectorAll<HTMLElement>('[data-org-refresh]').forEach(button=>button.onclick=async()=>{try{state=await call<OrgState>('org_refresh_connection',{connectionId:button.dataset.orgRefresh});ctx.notice(say('Список организаций обновлён','Organizations refreshed'));}catch(error){ctx.notice(orgErrorText(error),true);await refreshState();}ctx.render();});
  document.querySelectorAll<HTMLElement>('[data-org-disconnect]').forEach(button=>button.onclick=async()=>{
    const id=button.dataset.orgDisconnect!,pending=state.uploads.filter(item=>item.connectionId===id&&item.phase!=='done').length;
    const message=pending?say(`Ещё не загружено записей: ${pending}. Они останутся на устройстве — их можно будет сохранить локально.`,`${pending} recordings are not uploaded yet. They stay on this device and can be kept locally.`):say('Ключ доступа будет отозван и удалён с устройства. Записи на сервере останутся.','The access key will be revoked and removed from this device. Recordings on the server stay.');
    if(!await ctx.confirm(say('Отключить сервер?','Disconnect server?'),message,say('Отключить','Disconnect')))return;
    try{state=await call<OrgState>('org_disconnect',{connectionId:id});if(!cloudSource())setSource('local');}catch(error){ctx.notice(orgErrorText(error),true);}ctx.render();
  });
  document.querySelector<HTMLSelectElement>('#org-retention')?.addEventListener('change',async event=>{const value=(event.target as HTMLSelectElement).value;try{state=await call<OrgState>('org_set_retention',{days:value==='forever'?null:Number(value)});}catch(error){ctx.notice(orgErrorText(error),true);}ctx.render();});
  document.querySelector('#org-clear-cache')?.addEventListener('click',async()=>{if(!await ctx.confirm(say('Удалить аудио из облака с устройства?','Remove cloud audio from this device?'),say('Записи останутся на сервере, текст сохранится. Аудио загрузится снова при прослушивании.','Recordings stay on the server and text is kept. Audio downloads again when played.'),say('Очистить','Clear'),false))return;try{state=await call<OrgState>('org_clear_cache');}catch(error){ctx.notice(orgErrorText(error),true);}ctx.render();});
}
export function orgLoaded(){return loaded;}
