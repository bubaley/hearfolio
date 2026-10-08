import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {createRelay,BLOCK} from './relay.mjs';
async function setup(t,opts={}){const r=createRelay(opts);await new Promise(resolve=>r.server.listen(0,'127.0.0.1',resolve));t.after(()=>{r.server.closeAllConnections();r.server.close();});const base='http://127.0.0.1:'+r.server.address().port;const request=async(path,body,device)=>{const res=await fetch(base+path,{method:'POST',headers:{'content-type':'application/json',...(device?{authorization:'Bearer '+device.token}:{})},body:JSON.stringify(body)});return {status:res.status,body:await res.json()};};const register=async name=>(await request('/register',{name})).body;const rpc=(d,action,payload={})=>request('/rpc',{action,payload},d);return {...r,base,register,rpc};}
async function pair(r,a,b){const code=(await r.rpc(b,'code')).body.code;assert.equal((await r.rpc(a,'pair',{code})).status,200);assert.equal((await r.rpc(a,'pair',{code})).status,400);const id=(await r.rpc(b,'poll')).body.pairRequests[0].id;assert.equal((await r.rpc(b,'confirmPair',{id,accept:true})).status,200);}
function manifest(audio){return {version:1,sourceId:'hearing-test',name:'Meeting',extension:'wav',size:audio.length,sha256:createHash('sha256').update(audio).digest('hex'),transcript:'Сохранённая расшифровка',createdAt:123,completedAt:456,model:'whisper-base',configuration:{provider:'local',model:'whisper-base',mode:'local'}};}
test('pairing, binary relay, backpressure and saved acknowledgement',async t=>{const r=await setup(t);const a=await r.register('Laptop'),b=await r.register('Phone'),stranger=await r.register('Stranger');await pair(r,a,b);const audio=Buffer.alloc(BLOCK*2+17,123),m=manifest(audio);assert.equal((await r.rpc(stranger,'offer',{to:b.id,manifest:m})).status,403);const id=(await r.rpc(a,'offer',{to:b.id,manifest:m})).body.id;assert.equal((await r.rpc(a,'accept',{id})).status,400);const accepted=(await r.rpc(b,'accept',{id})).body;assert.deepEqual(accepted.manifest,m);const received=[];let offset=0;
 while(offset<audio.length){const block=audio.subarray(offset,offset+BLOCK);let resolved=false;const sending=fetch(r.base+`/transfers/${id}/chunk`,{method:'PUT',headers:{authorization:'Bearer '+a.token,'x-offset':String(offset)},body:block}).then(async res=>{resolved=true;assert.equal(res.status,200);return res.json();});while(!r.transfers.get(id).pending)await new Promise(resolve=>setTimeout(resolve,2));assert.equal(resolved,false);assert.equal(r.transfers.get(id).pending.buffer.length,block.length);const res=await fetch(r.base+`/transfers/${id}/chunk`,{headers:{authorization:'Bearer '+b.token}});assert.equal(Number(res.headers.get('x-offset')),offset);received.push(Buffer.from(await res.arrayBuffer()));offset+=block.length;assert.equal((await r.rpc(b,'ack',{id,offset})).status,200);assert.equal((await sending).offset,offset);}
 assert.deepEqual(Buffer.concat(received),audio);assert.equal((await r.rpc(a,'status',{id})).body.status,'streaming');assert.equal((await r.rpc(b,'finish',{id})).status,200);assert.equal((await r.rpc(a,'status',{id})).body.status,'delivered');assert.equal(r.transfers.get(id).pending,null);
});
test('invalid offsets and cancellation release a waiting sender',async t=>{const r=await setup(t);const a=await r.register('A'),b=await r.register('B');await pair(r,a,b);const id=(await r.rpc(a,'offer',{to:b.id,manifest:manifest(Buffer.alloc(10))})).body.id;await r.rpc(b,'accept',{id});const wrong=await fetch(r.base+`/transfers/${id}/chunk`,{method:'PUT',headers:{authorization:'Bearer '+a.token,'x-offset':'2'},body:Buffer.alloc(10)});assert.equal(wrong.status,409);const sending=fetch(r.base+`/transfers/${id}/chunk`,{method:'PUT',headers:{authorization:'Bearer '+a.token,'x-offset':'0'},body:Buffer.alloc(10)});while(!r.transfers.get(id).pending)await new Promise(resolve=>setTimeout(resolve,2));assert.equal((await r.rpc(b,'finish',{id})).status,400);await r.rpc(b,'cancel',{id});assert.equal((await sending).status,409);assert.equal(r.transfers.get(id).pending,null);});
test('expired codes, attempts and metadata validation',async t=>{const r=await setup(t,{codeTtl:10});const a=await r.register('A'),b=await r.register('B');const code=(await r.rpc(b,'code')).body.code;await new Promise(resolve=>setTimeout(resolve,20));assert.equal((await r.rpc(a,'pair',{code})).status,400);for(let i=0;i<10;i++)await r.rpc(a,'pair',{code:'00000000'});assert.equal((await r.rpc(a,'pair',{code:'00000000'})).status,429);const c=await r.register('C');await pair(r,c,b);assert.equal((await r.rpc(c,'offer',{to:b.id,manifest:{...manifest(Buffer.alloc(10)),extension:'../../wav'}})).status,400);await r.rpc(c,'unpair',{id:b.id});assert.equal((await r.rpc(c,'poll')).body.peers.length,0);assert.equal((await r.rpc(b,'poll')).body.peers.length,0);});
test('closed registration requires the bootstrap key; device requests still require individual tokens',async t=>{
 const r=await setup(t,{registrationKey:'fixture-access-key'});
 assert.equal((await r.register('No key')).error,'Access key required or invalid');
 const request=async(key)=>fetch(r.base+'/register',{method:'POST',headers:{'content-type':'application/json','x-hearfolio-access-key':key},body:JSON.stringify({name:'Allowed device'})});
 assert.equal((await request('wrong-key')).status,401);
 const response=await request('fixture-access-key');assert.equal(response.status,200);const device=await response.json();
 assert.equal((await r.rpc(device,'poll')).status,200);
 assert.equal((await r.rpc({token:'fixture-access-key'},'poll')).status,401);
});
test('invitation registers and pairs a new device only after owner approval',async t=>{
 const r=await setup(t,{registrationKey:'private-fixture'});
 const ownerResponse=await fetch(r.base+'/register',{method:'POST',headers:{'content-type':'application/json','x-hearfolio-access-key':'private-fixture'},body:JSON.stringify({name:'Owner'})});const owner=await ownerResponse.json();
 const post=async(path,data)=>{const res=await fetch(r.base+path,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(data)});return {status:res.status,body:await res.json()};};
 const code=(await r.rpc(owner,'code')).body.code;
 const joined=await post('/join',{name:'New Android',code});assert.equal(joined.status,200);assert.equal(joined.body.token,undefined);
 assert.equal((await post('/join',{name:'Replay',code})).status,400);
 assert.equal((await post('/join/status',{...joined.body,secret:'wrong'})).status,404);
 assert.deepEqual((await post('/join/status',joined.body)).body,{status:'waiting'});
 const inbox=(await r.rpc(owner,'poll')).body;assert.equal(inbox.peers.length,0);assert.equal(inbox.pairRequests[0].name,'New Android');
 await r.rpc(owner,'confirmPair',{id:inbox.pairRequests[0].id,accept:true});
 const approved=(await post('/join/status',joined.body)).body;assert.equal(approved.status,'approved');assert.ok(approved.device.token);
 assert.deepEqual((await post('/join/status',joined.body)).body,approved); // Lost response may be safely fetched again.
 assert.equal((await r.rpc(approved.device,'poll')).body.peers[0].id,owner.id);
 assert.equal((await r.rpc(owner,'poll')).body.peers[0].id,approved.device.id);
 assert.equal((await r.register('Still requires key')).error,'Access key required or invalid');
});
test('rejected and expired invitations never grant device credentials',async t=>{
 const r=await setup(t,{codeTtl:100});const owner=await r.register('Owner');
 const post=async(path,data)=>{const res=await fetch(r.base+path,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(data)});return {status:res.status,body:await res.json()};};
 const code=(await r.rpc(owner,'code')).body.code;const ticket=(await post('/join',{name:'Rejected',code})).body;
 const request=(await r.rpc(owner,'poll')).body.pairRequests[0];await r.rpc(owner,'confirmPair',{id:request.id,accept:false});
 assert.deepEqual((await post('/join/status',ticket)).body,{status:'rejected'});assert.equal((await r.rpc(owner,'poll')).body.peers.length,0);
 const expiring=(await r.rpc(owner,'code')).body.code;await new Promise(resolve=>setTimeout(resolve,120));
 assert.equal((await post('/join',{name:'Expired',code:expiring})).status,400);
 assert.equal((await post('/join/status',ticket)).status,404);
});
