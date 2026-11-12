// Pure presentation rules for organization recordings: what a row says and
// which action it offers. No DOM, no Tauri — covered by org-status.test.mjs.

export type UploadPhase='queued'|'uploading'|'completing'|'done'|'failed';
export type UploadView={clientId:string;connectionId:string;organizationId:string;remoteId:string|null;displayName:string;recordedAt:number;durationMs:number|null;size:number;uploaded:number;phase:UploadPhase;error:string|null;waitingNetwork:boolean;nextAttemptAt:number;projectId:string|null;visibility:string;localRecordId:string|null;hasTranscript:boolean;createdAt:number};
export type CloudRecording={id:string;client_id?:string;title?:string;recorded_at?:string;duration_ms?:number;status?:string;upload_state?:string;progress?:number;segments_done?:number;segments_total?:number;error_message?:string|null;project?:{id:string;name:string;emoji?:string|null}|null;tags?:{id:string;name:string;color:string}[];visibility?:string;transcript_words?:number;results_count?:number;transcript_text?:string;language?:string|null;created_by?:{first_name?:string;last_name?:string;email?:string}|null};
export type Tone='queued'|'progress'|'error'|'done'|'muted';
export type Status={label:string;tone:Tone;percent:number|null;action:'retry-upload'|'retry-transcription'|null};
type Say=(ru:string,en:string)=>string;

export function uploadPercent(upload:UploadView):number {
  if(upload.phase==='completing'||upload.phase==='done')return 100;
  return upload.size>0?Math.min(100,Math.floor(upload.uploaded/upload.size*100)):0;
}

/** Status of a recording that has not fully reached the server yet. */
export function uploadStatus(upload:UploadView,say:Say):Status {
  if(upload.phase==='failed'){
    // A removed connection cannot be retried; an expired sign-in can (it also resumes by itself after signing in again).
    if(upload.error==='errors.orgDisconnected')return {label:say('Нет доступа к организации','No access to the organization'),tone:'error',percent:null,action:null};
    if(upload.error==='errors.orgUnauthorized')return {label:say('Нужно войти снова','Sign in again'),tone:'error',percent:null,action:'retry-upload'};
    return {label:say('Ошибка загрузки','Upload failed'),tone:'error',percent:null,action:'retry-upload'};
  }
  if(upload.waitingNetwork)return {label:say('Ждёт сети','Waiting for network'),tone:'queued',percent:null,action:null};
  if(upload.phase==='queued')return {label:say('В очереди','Queued'),tone:'queued',percent:null,action:null};
  const percent=uploadPercent(upload);
  if(upload.phase==='uploading'&&upload.uploaded===0&&upload.error)return {label:say('Повторяем загрузку','Retrying upload'),tone:'queued',percent:0,action:null};
  return {label:`${say('Загружается','Uploading')} ${percent}%`,tone:'progress',percent,action:null};
}

/** Status reported by the server for an uploaded recording. */
export function recordingStatus(item:CloudRecording,say:Say):Status {
  switch(item.status){
    case 'awaiting_upload':return item.upload_state==='failed'?{label:say('Ошибка загрузки','Upload failed'),tone:'error',percent:null,action:null}:{label:say('Ожидает аудио','Waiting for audio'),tone:'queued',percent:null,action:null};
    case 'queued':return {label:say('В очереди на расшифровку','Queued for transcription'),tone:'queued',percent:null,action:null};
    case 'transcribing':{
      const total=item.segments_total||0,done=item.segments_done||0;
      const percent=total>0?Math.floor(done/total*100):Math.max(0,Math.min(100,item.progress||0));
      return {label:total>0?`${say('Расшифровывается','Transcribing')} ${done}/${total}`:`${say('Расшифровывается','Transcribing')} ${percent}%`,tone:'progress',percent,action:null};
    }
    case 'failed':return {label:say('Ошибка расшифровки','Transcription failed'),tone:'error',percent:null,action:'retry-transcription'};
    case 'transcribed':return {label:say('Готово','Ready'),tone:'done',percent:null,action:null};
    default:return {label:say('Обрабатывается','Processing'),tone:'muted',percent:null,action:null};
  }
}

export type Row={key:string;title:string;recordedAt:number;durationMs:number|null;item:CloudRecording|null;upload:UploadView|null;status:Status};

/**
 * One list for an organization: synced recordings plus uploads the server does
 * not list yet. An unfinished upload outranks the server status of its recording.
 */
export function mergeRows(items:CloudRecording[],uploads:UploadView[],connectionId:string,organizationId:string,say:Say):Row[] {
  const mine=uploads.filter(upload=>upload.connectionId===connectionId&&upload.organizationId===organizationId);
  const byRemote=new Map(mine.filter(upload=>upload.remoteId).map(upload=>[upload.remoteId!,upload]));
  const byClient=new Map(mine.map(upload=>[upload.clientId,upload]));
  const seen=new Set<string>();
  const rows:Row[]=items.map(item=>{
    const upload=byRemote.get(item.id)||(item.client_id?byClient.get(item.client_id):undefined)||null;
    if(upload)seen.add(upload.clientId);
    const pending=upload&&upload.phase!=='done';
    return {key:item.id,title:item.title||upload?.displayName||say('Без названия','Untitled'),recordedAt:Date.parse(item.recorded_at||'')||upload?.recordedAt||0,durationMs:item.duration_ms??upload?.durationMs??null,item,upload,status:pending?uploadStatus(upload!,say):recordingStatus(item,say)};
  });
  for(const upload of mine)if(!seen.has(upload.clientId)&&upload.phase!=='done')rows.push({key:`upload:${upload.clientId}`,title:upload.displayName,recordedAt:upload.recordedAt,durationMs:upload.durationMs,item:null,upload,status:uploadStatus(upload,say)});
  return rows.sort((left,right)=>right.recordedAt-left.recordedAt);
}

export const RETENTION_CHOICES:(number|null)[]=[7,30,90,null];
