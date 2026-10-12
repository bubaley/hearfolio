import {test} from 'node:test';
import assert from 'node:assert/strict';
import {automaticActions} from '../src/transfer-policy.ts';
const inbox={pairRequests:[{id:'new-device'}],offers:[{id:'recording-a'},{id:'recording-b'}]};
test('confirmation remains required by default; the two preferences act independently',()=>{
 assert.deepEqual(automaticActions(inbox,{autoPair:false,autoReceive:false},false,new Set()),{pair:[],receive:null});
 assert.deepEqual(automaticActions(inbox,{autoPair:true,autoReceive:false},false,new Set()),{pair:['new-device'],receive:null});
 assert.deepEqual(automaticActions(inbox,{autoPair:false,autoReceive:true},false,new Set()),{pair:[],receive:'recording-a'});
});
test('busy recording or transcription defers receiving; pending offers resume when idle',()=>{
 const settings={autoPair:true,autoReceive:true};
 assert.equal(automaticActions(inbox,settings,true,new Set()).receive,null);
 assert.equal(automaticActions(inbox,settings,false,new Set()).receive,'recording-a');
});
test('failed or in-flight automatic actions are not retried every poll; other files can proceed',()=>{
 const attempted=new Set(['pair:new-device','receive:recording-a']);
 assert.deepEqual(automaticActions(inbox,{autoPair:true,autoReceive:true},false,attempted),{pair:[],receive:'recording-b'});
 assert.equal(automaticActions(inbox,{autoPair:true,autoReceive:false},false,attempted).receive,null);
});

test('fresh transfer setup has no implicit server; explicit domains remain configured',async()=>{
 const {configuredServer}=await import('../src/transfer-policy.ts');
 assert.equal(configuredServer(null),'');
 assert.equal(configuredServer('   '),'');
 assert.equal(configuredServer(' https://my-relay.example/ '),'https://my-relay.example');
 assert.equal(configuredServer('https://hearfolio.corextech.ru'),'https://hearfolio.corextech.ru');
});
test('invitation expiry reflects the actual server deadline and never goes negative',async()=>{
 const {invitationSeconds}=await import('../src/transfer-policy.ts');
 assert.equal(invitationSeconds(180000,1000),179);
 assert.equal(invitationSeconds(180000,179001),1);
 assert.equal(invitationSeconds(180000,180000),0);
 assert.equal(invitationSeconds(180000,180001),0);
});
