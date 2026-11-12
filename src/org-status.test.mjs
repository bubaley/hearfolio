import {test} from 'node:test';
import assert from 'node:assert/strict';
import {uploadStatus, recordingStatus, mergeRows, uploadPercent} from './org-status.ts';

const say=(ru)=>ru;
const upload=(patch={})=>({clientId:'c1',connectionId:'conn',organizationId:'org',remoteId:null,displayName:'Запись 1',recordedAt:1000,durationMs:5000,size:200,uploaded:0,phase:'queued',error:null,waitingNetwork:false,nextAttemptAt:0,projectId:null,visibility:'organization',localRecordId:null,hasTranscript:false,createdAt:1,...patch});

test('upload states read like the product spec',()=>{
  assert.equal(uploadStatus(upload(),say).label,'В очереди');
  assert.equal(uploadStatus(upload({phase:'uploading',uploaded:90}),say).label,'Загружается 45%');
  assert.equal(uploadPercent(upload({phase:'completing',uploaded:10})),100);
  assert.equal(uploadStatus(upload({waitingNetwork:true,phase:'uploading'}),say).label,'Ждёт сети');
  const failed=uploadStatus(upload({phase:'failed',error:'errors.orgServer'}),say);
  assert.deepEqual([failed.label,failed.tone,failed.action],['Ошибка загрузки','error','retry-upload']);
  assert.equal(uploadStatus(upload({phase:'failed',error:'errors.orgDisconnected'}),say).action,null);
});

test('server statuses show transcription progress and failures',()=>{
  assert.equal(recordingStatus({id:'r',status:'transcribing',segments_done:3,segments_total:8},say).label,'Расшифровывается 3/8');
  assert.equal(recordingStatus({id:'r',status:'transcribing',progress:40},say).label,'Расшифровывается 40%');
  assert.equal(recordingStatus({id:'r',status:'transcribed'},say).label,'Готово');
  assert.equal(recordingStatus({id:'r',status:'failed'},say).action,'retry-transcription');
  assert.equal(recordingStatus({id:'r',status:'something-new'},say).tone,'muted');
});

test('rows merge synced recordings with pending uploads without duplicates',()=>{
  const items=[{id:'r1',client_id:'c1',title:'',recorded_at:'2026-01-02T00:00:00Z',status:'awaiting_upload'},{id:'r2',title:'Старая',recorded_at:'2026-01-01T00:00:00Z',status:'transcribed'}];
  const uploads=[upload({remoteId:'r1',phase:'uploading',uploaded:100}),upload({clientId:'c2',displayName:'Новая',recordedAt:Date.parse('2026-01-03T00:00:00Z')}),upload({clientId:'c3',organizationId:'other'}),upload({clientId:'c4',phase:'done',remoteId:'gone'})];
  const rows=mergeRows(items,uploads,'conn','org',say);
  assert.deepEqual(rows.map(row=>row.key),['upload:c2','r1','r2']);
  assert.equal(rows[1].title,'Запись 1');
  assert.equal(rows[1].status.label,'Загружается 50%');
  assert.equal(rows[2].status.label,'Готово');
});
