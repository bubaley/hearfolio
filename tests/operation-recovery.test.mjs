import test from 'node:test';
import assert from 'node:assert/strict';
import {OperationMailbox} from '../src/operation-recovery.ts';

test('stale native result cannot complete a newly requested job', () => {
  const box=new OperationMailbox(), values=[];
  box.begin({kind:'transcribe',recordId:'audio',excludedId:'old',resolve:value=>values.push(value),reject:error=>values.push(error)});
  assert.equal(box.observe({id:'old',kind:'transcribe',recordId:'audio',state:'finished',result:'old text'}).matched,false);
  assert.equal(box.observe({id:'new',kind:'postprocess',recordId:'audio',state:'running'}).matched,false);
  assert.equal(box.observe({id:'new',kind:'transcribe',recordId:'audio',state:'running'}).matched,true);
  assert.equal(box.observe({id:'other',kind:'transcribe',recordId:'audio',state:'finished',result:'wrong text'}).matched,false);
  assert.equal(box.observe({id:'new',kind:'transcribe',recordId:'audio',state:'finished',result:'saved text'}).terminal,true);
  assert.deepEqual(values,['saved text']);
  assert.equal(box.observe({id:'new',kind:'transcribe',recordId:'audio',state:'finished',result:'duplicate'}).matched,false);
});

test('terminal snapshot replaces a lost invoke callback without replaying the request', () => {
  const box=new OperationMailbox(), errors=[];
  const release=box.begin({kind:'import',recordId:null,excludedId:'',resolve:()=>assert.fail('not finished'),reject:error=>errors.push(error.message)});
  assert.equal(box.observe({id:'import-1',kind:'import',recordId:null,state:'interrupted',error:'errors.operationInterrupted'}).terminal,true);
  release();
  assert.deepEqual(errors,['errors.operationInterrupted']);
});
