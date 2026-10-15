import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import http from 'node:http';
import {createRelay} from './relay.mjs';
import {PART_SIZE,PART_WINDOW} from './multipart.mjs';
const hash=b=>createHash('sha256').update(b).digest('hex');
const wait=async predicate=>{const end=Date.now()+3000;while(!predicate()){assert.ok(Date.now()<end,'condition timed out');await new Promise(r=>setTimeout(r,5));}};
async function setup(t){
  const r=createRelay();await new Promise(resolve=>r.server.listen(0,'127.0.0.1',resolve));t.after(()=>{r.server.closeAllConnections();r.server.close();});
  r.base='http://127.0.0.1:'+r.server.address().port;
  r.rpc=async(d,action,payload={})=>{const response=await fetch(r.base+'/rpc',{method:'POST',headers:{authorization:'Bearer '+d.token,'content-type':'application/json'},body:JSON.stringify({action,payload})});return {status:response.status,body:await response.json()};};
  r.device=async name=>{const response=await fetch(r.base+'/register',{method:'POST',body:JSON.stringify({name})});return response.json();};
  r.pair=async(a,b)=>{const code=(await r.rpc(b,'code')).body.code;await r.rpc(a,'pair',{code});const poll=(await r.rpc(b,'poll')).body;await r.rpc(b,'confirmPair',{id:poll.pairRequests[0].id,accept:true});};
  r.offer=async(a,b,blocks)=>{const audio=Buffer.concat(blocks),parts={size:PART_SIZE,hashes:blocks.map(hash)};return r.rpc(a,'offer',{to:b.id,parts,manifest:{version:1,name:'fixture.wav',extension:'wav',sourceId:'fixture',size:audio.length,sha256:hash(audio),transcript:'',createdAt:1}});};
  r.put=(a,id,index,bytes,options={})=>fetch(`${r.base}/transfers/${id}/parts/${index}`,{method:'PUT',headers:{authorization:'Bearer '+a.token,'x-part-sha256':hash(bytes)},body:bytes,...options});
  r.get=(b,id,index)=>fetch(`${r.base}/transfers/${id}/parts/${index}`,{headers:{authorization:'Bearer '+b.token}});
  return r;
}
test('four parts are in flight; out-of-order durable acknowledgements are idempotent',async t=>{
  const r=await setup(t),a=await r.device('A'),b=await r.device('B');await r.pair(a,b);
  const blocks=Array.from({length:4},(_,i)=>Buffer.alloc(PART_SIZE,i+1));const id=(await r.offer(a,b,blocks)).body.id;
  assert.equal((await r.rpc(b,'accept',{id,parts:{verified:[]}})).status,200);
  const uploading=blocks.map((bytes,i)=>r.put(a,id,i,bytes));await wait(()=>r.transfers.get(id).parts.pending.size===4&&[...r.transfers.get(id).parts.pending.values()].every(p=>p.buffer));
  for(const i of [3,1,0,2]){
    const response=await r.get(b,id,i);assert.equal(response.status,200);assert.equal(hash(Buffer.from(await response.arrayBuffer())),hash(blocks[i]));
    const payload={id,index:i,sha256:hash(blocks[i])};assert.equal((await r.rpc(b,'ackPart',payload)).status,200);assert.equal((await r.rpc(b,'ackPart',payload)).status,200);
  }
  assert.deepEqual((await Promise.all(uploading)).map(r=>r.status),[200,200,200,200]);
  assert.equal((await r.put(a,id,0,blocks[0])).status,200);
  assert.equal((await r.rpc(a,'status',{id})).body.offset,PART_SIZE*4);
  assert.equal((await r.rpc(b,'finish',{id})).status,200);assert.equal(r.transfers.get(id).parts.pending.size,0);
});
test('lost uploader connection keeps a part and retry reattaches without cancelling the session',async t=>{
  const r=await setup(t),a=await r.device('A'),b=await r.device('B');await r.pair(a,b);
  const bytes=Buffer.alloc(37,9),id=(await r.offer(a,b,[bytes])).body.id;await r.rpc(b,'accept',{id,parts:{verified:[]}});
  const abort=new AbortController();const first=r.put(a,id,0,bytes,{signal:abort.signal}).catch(()=>null);
  await wait(()=>r.transfers.get(id).parts.pending.get(0)?.buffer);abort.abort();await first;await wait(()=>r.transfers.get(id).parts.pending.get(0)?.res===null);
  const second=r.put(a,id,0,bytes);await wait(()=>r.transfers.get(id).parts.pending.get(0)?.res);
  assert.equal((await r.rpc(b,'ackPart',{id,index:0,sha256:hash(bytes)})).status,200);assert.equal((await second).status,200);
  assert.equal((await r.rpc(a,'status',{id})).body.status,'streaming');
});
test('resume skips verified parts; corrupt bytes and invalid checkpoints are rejected',async t=>{
  const r=await setup(t),a=await r.device('A'),b=await r.device('B');await r.pair(a,b);
  const blocks=[Buffer.alloc(PART_SIZE,1),Buffer.alloc(21,2)],id=(await r.offer(a,b,blocks)).body.id;
  assert.equal((await r.rpc(b,'accept',{id,parts:{verified:[0,0]}})).status,400);
  assert.equal((await r.rpc(b,'accept',{id,parts:{verified:[2]}})).status,400);
  assert.equal((await r.rpc(b,'accept',{id,parts:{verified:[0]}})).status,200);
  assert.deepEqual((await r.rpc(a,'status',{id})).body.parts.missing,[1]);
  const wrong=await fetch(`${r.base}/transfers/${id}/parts/1`,{method:'PUT',headers:{authorization:'Bearer '+a.token,'x-part-sha256':hash(blocks[1])},body:Buffer.alloc(21,3)});
  assert.equal(wrong.status,422);assert.equal(r.transfers.get(id).parts.pending.size,0);
  assert.equal((await r.rpc(b,'finish',{id})).status,400);
  const sending=r.put(a,id,1,blocks[1]);await wait(()=>r.transfers.get(id).parts.pending.get(1)?.buffer);await r.rpc(b,'ackPart',{id,index:1,sha256:hash(blocks[1])});await sending;
  assert.equal((await r.rpc(b,'finish',{id})).status,200);
});
test('one device has bounded slots and sessions while another device can transfer concurrently',async t=>{
  const r=await setup(t),a=await r.device('A'),b=await r.device('B'),c=await r.device('C'),d=await r.device('D'),e=await r.device('E'),f=await r.device('F');
  await r.pair(a,b);await r.pair(a,c);await r.pair(a,d);await r.pair(e,f);
  const blocks=Array.from({length:5},()=>Buffer.alloc(PART_SIZE,1));const id=(await r.offer(a,b,blocks)).body.id;await r.rpc(b,'accept',{id,parts:{verified:[]}});
  const uploads=blocks.slice(0,4).map((bytes,i)=>r.put(a,id,i,bytes));await wait(()=>r.transfers.get(id).parts.pending.size===PART_WINDOW);
  assert.equal((await r.put(a,id,4,blocks[4])).status,429);
  const other=(await r.offer(a,c,[Buffer.alloc(3)])).body.id;await r.rpc(c,'accept',{id:other,parts:{verified:[]}});
  assert.equal((await r.put(a,other,0,Buffer.alloc(3))).status,429);
  assert.equal((await r.offer(a,d,[Buffer.alloc(3)])).status,429);
  await r.pair(e,a);assert.equal((await r.offer(e,a,[Buffer.alloc(3)])).status,429);
  const independent=(await r.offer(e,f,[Buffer.alloc(3)])).body.id;await r.rpc(f,'accept',{id:independent,parts:{verified:[]}});
  const independentUpload=r.put(e,independent,0,Buffer.alloc(3));await wait(()=>r.transfers.get(independent).parts.pending.get(0)?.buffer);
  await r.rpc(f,'ackPart',{id:independent,index:0,sha256:hash(Buffer.alloc(3))});assert.equal((await independentUpload).status,200);
  await r.rpc(a,'cancel',{id});await Promise.all(uploads);
});
test('old receivers can use the legacy stream despite a multipart-capable sender',async t=>{
  const r=await setup(t),a=await r.device('A'),b=await r.device('B');await r.pair(a,b);
  const id=(await r.offer(a,b,[Buffer.alloc(7)])).body.id;const accepted=await r.rpc(b,'accept',{id});
  assert.equal(accepted.body.parts,null);assert.equal((await r.rpc(a,'status',{id})).body.parts,null);
});
test('global reservation is bounded before reading bodies and cancel releases capacity',async t=>{
  const r=await setup(t),blocks=Array.from({length:4},()=>Buffer.alloc(PART_SIZE,5)),sessions=[];
  for(let i=0;i<9;i++){
    const a=await r.device('Sender '+i),b=await r.device('Receiver '+i);await r.pair(a,b);
    const id=(await r.offer(a,b,blocks)).body.id;await r.rpc(b,'accept',{id,parts:{verified:[]}});
    const uploads=i<8?blocks.map((bytes,index)=>r.put(a,id,index,bytes)):[];sessions.push({a,b,id,uploads});
    if(i<8)await wait(()=>r.transfers.get(id).parts.pending.size===4);
  }
  const ninth=sessions[8];assert.equal((await r.put(ninth.a,ninth.id,0,blocks[0])).status,429);
  assert.equal([...r.transfers.values()].reduce((n,t)=>n+t.parts.pending.size,0),32);
  await r.rpc(sessions[0].a,'cancel',{id:sessions[0].id});await Promise.all(sessions[0].uploads);
  const next=r.put(ninth.a,ninth.id,0,blocks[0]);await wait(()=>r.transfers.get(ninth.id).parts.pending.get(0)?.buffer);
  await r.rpc(ninth.b,'ackPart',{id:ninth.id,index:0,sha256:hash(blocks[0])});assert.equal((await next).status,200);
  for(const s of sessions.slice(1,8)){await r.rpc(s.a,'cancel',{id:s.id});await Promise.all(s.uploads);}
});

test('one authenticated device cannot occupy the connection budget of another',async t=>{
  const r=await setup(t),a=await r.device('A'),b=await r.device('B');
  const slow=Array.from({length:8},()=>{const req=http.request(r.base+'/rpc',{method:'POST',headers:{authorization:'Bearer '+a.token,'content-type':'application/json'}},res=>res.resume());req.on('error',()=>{});req.write('{');return req;});
  await wait(()=>r.activeRequests.get(a.id)===8);
  assert.equal((await r.rpc(a,'poll')).status,429);
  assert.equal((await r.rpc(b,'poll')).status,200);
  for(const req of slow)req.destroy();await wait(()=>!r.activeRequests.has(a.id));
  assert.equal((await r.rpc(a,'poll')).status,200);
});
