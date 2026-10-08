import http from 'node:http';
import {randomBytes, randomInt, randomUUID, createHash, timingSafeEqual} from 'node:crypto';
import {readFileSync, writeFileSync, renameSync, mkdirSync} from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

export const BLOCK=65536, MAX_SIZE=1024*1024*1024;
export function createRelay({stateFile, ttl=120000, codeTtl=180000, registrationKey=''}={}) {
  let devices=[];
  if(stateFile){mkdirSync(path.dirname(stateFile),{recursive:true});try{devices=JSON.parse(readFileSync(stateFile,'utf8'));}catch(e){if(e.code!=='ENOENT')throw e;}}
  const online=new Map(), codes=new Map(), attempts=new Map(), pairs=new Map(), joins=new Map(), transfers=new Map();
  let joinAttempts=0, joinWindow=Date.now();
  const persist=()=>{if(stateFile){writeFileSync(stateFile+'.tmp',JSON.stringify(devices),{mode:0o600});renameSync(stateFile+'.tmp',stateFile);}};
  const fail=(message,status=400)=>{throw Object.assign(new Error(message),{status});};
  const text=(value,max=200)=>typeof value==='string'&&value.length>0&&value.length<=max&&!/[\x00-\x1f]/.test(value);
  const isOnline=id=>Date.now()-(online.get(id)||0)<10000;
  const json=(res,body,status=200)=>{if(!res.destroyed){res.writeHead(status,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify(body));}};
  const read=async(req,max)=>{const parts=[];let size=0;for await(const part of req){size+=part.length;if(size>max)fail('Request too large',413);parts.push(part);}return Buffer.concat(parts);};
  const stop=t=>{if(t.pending){json(t.pending.res,{error:'Transfer cancelled'},409);t.pending=null;}t.status='cancelled';t.updated=Date.now();};
  const timer=setInterval(()=>{
    const now=Date.now();for(const [id,t] of transfers)if(now-t.updated>ttl){stop(t);transfers.delete(id);}
    for(const [code,c] of codes)if(c.until<now)codes.delete(code);
    for(const [id,p] of pairs)if(p.until<now)pairs.delete(id);
    for(const [id,j] of joins)if(j.until<now)joins.delete(id);
    for(const [id,a] of attempts)if(a.until<now)attempts.delete(id);
  },1000);timer.unref();
  const server=http.createServer(async(req,res)=>{
    try {
      const url=new URL(req.url,'http://relay');
      if(req.method==='GET'&&url.pathname==='/health')return json(res,{ok:true,protocol:1});
      if(req.method==='POST'&&url.pathname==='/join'){
        if(Date.now()-joinWindow>60000){joinWindow=Date.now();joinAttempts=0;}
        if(++joinAttempts>60)fail('Too many invitations; wait one minute',429);
        const data=JSON.parse((await read(req,1024)).toString());
        if(!text(data.name,100)||!/^\d{8}$/.test(data.code))fail('Invalid invitation');
        const c=codes.get(data.code);if(!c||c.until<Date.now())fail('Invalid or expired invitation');
        if(devices.length+[...joins.values()].filter(j=>j.status==='waiting').length>=100)fail('Device limit reached',429);
        if(joins.size>=20)fail('Too many pending invitations',429);
        codes.delete(data.code);const id=randomUUID(),secret=randomBytes(32).toString('hex'),until=Date.now()+codeTtl;
        joins.set(id,{name:data.name,to:c.to,secret,until,status:'waiting'});
        pairs.set(id,{to:c.to,join:id,until});return json(res,{id,secret});
      }
      if(req.method==='POST'&&url.pathname==='/join/status'){
        const data=JSON.parse((await read(req,1024)).toString()),j=joins.get(data.id);
        if(!j||j.until<Date.now()||typeof data.secret!=='string'||!timingSafeEqual(createHash('sha256').update(data.secret).digest(),createHash('sha256').update(j.secret).digest()))fail('Invitation expired or invalid',404);
        return json(res,{status:j.status,...(j.status==='approved'?{device:j.device}:{})});
      }
      if(req.method==='POST'&&url.pathname==='/register'){
        if(registrationKey){const supplied=req.headers['x-hearfolio-access-key'];if(typeof supplied!=='string'||!timingSafeEqual(createHash('sha256').update(supplied).digest(),createHash('sha256').update(registrationKey).digest()))fail('Access key required or invalid',401);}
        const data=JSON.parse((await read(req,1024)).toString());if(!text(data.name))fail('Invalid device name');
        if(devices.length>=100)fail('Device limit reached',429);
        const device={id:randomUUID(),token:randomBytes(32).toString('hex'),name:data.name,peers:[]};devices.push(device);persist();online.set(device.id,Date.now());return json(res,{id:device.id,token:device.token});
      }
      const token=req.headers.authorization?.replace(/^Bearer /,'');const device=devices.find(d=>d.token===token);
      if(!device)fail('Unauthorized',401);online.set(device.id,Date.now());
      if(req.method==='POST'&&url.pathname==='/rpc'){
        const {action,payload={}}=JSON.parse((await read(req,5*1024*1024)).toString());
        if(action==='poll')return json(res,{device:{id:device.id,name:device.name},peers:devices.filter(d=>device.peers.includes(d.id)).map(d=>({id:d.id,name:d.name,online:isOnline(d.id)})),pairRequests:[...pairs.entries()].filter(([,p])=>p.to===device.id).map(([id,p])=>({id,name:p.join?joins.get(p.join)?.name:devices.find(d=>d.id===p.from)?.name})),offers:[...transfers.values()].filter(t=>t.to===device.id&&t.status==='offered').map(t=>({id:t.id,name:t.manifest.name,size:t.manifest.size,from:devices.find(d=>d.id===t.from)?.name})),transfers:[...transfers.values()].filter(t=>t.from===device.id||t.to===device.id).map(t=>({id:t.id,status:t.status,offset:t.offset,size:t.manifest.size}))});
        if(action==='rename'){if(!text(payload.name))fail('Invalid device name');device.name=payload.name;persist();return json(res,{ok:true});}
        if(action==='code'){
          for(const [c,p] of codes)if(p.to===device.id)codes.delete(c);
          let code;do{code=String(randomInt(10000000,100000000));}while(codes.has(code));codes.set(code,{to:device.id,until:Date.now()+codeTtl});return json(res,{code,expiresAt:Date.now()+codeTtl});
        }
        if(action==='pair'){
          let a=attempts.get(device.id);if(!a){a={count:0,until:Date.now()+60000};attempts.set(device.id,a);}if(++a.count>10)fail('Too many attempts; wait one minute',429);
          const c=codes.get(payload.code);if(!c||c.until<Date.now()||c.to===device.id)fail('Invalid or expired code');codes.delete(payload.code);const id=randomUUID();pairs.set(id,{from:device.id,to:c.to,until:Date.now()+codeTtl});return json(res,{ok:true});
        }
        if(action==='confirmPair'){
          const p=pairs.get(payload.id);if(!p||p.to!==device.id||p.until<Date.now())fail('Pair request expired');pairs.delete(payload.id);
          if(p.join){
            const j=joins.get(p.join);if(!j||j.until<Date.now())fail('Invitation expired');
            if(payload.accept){
              if(devices.length>=100)fail('Device limit reached',429);
              const other={id:randomUUID(),token:randomBytes(32).toString('hex'),name:j.name,peers:[device.id]};
              devices.push(other);device.peers.push(other.id);persist();j.device={id:other.id,token:other.token};j.status='approved';online.set(other.id,Date.now());
            }else j.status='rejected';
            return json(res,{ok:true});
          }
          if(payload.accept){const other=devices.find(d=>d.id===p.from);if(!device.peers.includes(other.id))device.peers.push(other.id);if(!other.peers.includes(device.id))other.peers.push(device.id);persist();}return json(res,{ok:true});
        }
        if(action==='unpair'){
          device.peers=device.peers.filter(id=>id!==payload.id);const other=devices.find(d=>d.id===payload.id);if(other)other.peers=other.peers.filter(id=>id!==device.id);for(const t of transfers.values())if([t.from,t.to].includes(device.id)&&[t.from,t.to].includes(payload.id))stop(t);persist();return json(res,{ok:true});
        }
        if(action==='offer'){
          if(!device.peers.includes(payload.to))fail('Device is not paired',403);if(!isOnline(payload.to))fail('Recipient is offline',409);
          const m=payload.manifest;
          if(!m||m.version!==1||!text(m.name)||!text(m.sourceId)||!['m4a','mp3','wav','mp4','aac','flac','ogg'].includes(m.extension)||!Number.isSafeInteger(m.size)||m.size<=0||m.size>MAX_SIZE||!/^[a-f0-9]{64}$/.test(m.sha256)||typeof m.transcript!=='string'||Buffer.byteLength(m.transcript)>4*1024*1024||!Number.isSafeInteger(m.createdAt)||m.createdAt<0)fail('Invalid recording manifest');
          if([...transfers.values()].filter(t=>!['delivered','cancelled'].includes(t.status)).length>=16)fail('Relay busy',429);
          if([...transfers.values()].some(t=>t.to===payload.to&&!['delivered','cancelled'].includes(t.status)))fail('Recipient has another transfer',409);
          const id=randomUUID();transfers.set(id,{id,from:device.id,to:payload.to,manifest:m,status:'offered',offset:0,updated:Date.now(),pending:null});return json(res,{id});
        }
        const t=transfers.get(payload.id);if(!t||![t.from,t.to].includes(device.id))fail('Unknown transfer',404);t.updated=Date.now();
        if(action==='status')return json(res,{status:t.status,offset:t.offset,size:t.manifest.size});
        if(action==='accept'&&t.to===device.id&&t.status==='offered'){t.status='streaming';return json(res,{manifest:t.manifest,from:t.from});}
        if(action==='cancel'){stop(t);return json(res,{ok:true});}
        if(action==='ack'&&t.to===device.id&&t.status==='streaming'){
          if(!t.pending||payload.offset!==t.offset+t.pending.buffer.length)fail('Invalid acknowledgement');const pending=t.pending;t.pending=null;t.offset=payload.offset;json(pending.res,{offset:t.offset});return json(res,{ok:true});
        }
        if(action==='finish'&&t.to===device.id&&t.status==='streaming'&&t.offset===t.manifest.size){t.status='delivered';t.manifest.transcript='';return json(res,{ok:true});}
        fail('Invalid action');
      }
      const match=/^\/transfers\/([a-f0-9-]+)\/chunk$/.exec(url.pathname);const t=match&&transfers.get(match[1]);
      if(!t||![t.from,t.to].includes(device.id))fail('Unknown transfer',404);t.updated=Date.now();if(t.status!=='streaming')fail('Transfer is not active',409);
      if(req.method==='PUT'&&t.from===device.id){
        if(t.pending)fail('A block is already pending',409);if(Number(req.headers['x-offset'])!==t.offset)fail('Invalid offset',409);
        const buffer=await read(req,BLOCK);if(!buffer.length||t.offset+buffer.length>t.manifest.size)fail('Invalid block');
        if(t.status!=='streaming'||t.pending)fail('Transfer changed',409);t.pending={buffer,res};res.on('close',()=>{if(t.pending?.res===res)stop(t);});return;
      }
      if(req.method==='GET'&&t.to===device.id){
        if(!t.pending){res.writeHead(204);res.end();return;}res.writeHead(200,{'content-type':'application/octet-stream','x-offset':String(t.offset),'content-length':t.pending.buffer.length,'cache-control':'no-store'});res.end(t.pending.buffer);return;
      }
      fail('Not found',404);
    } catch(e){json(res,{error:e.status?e.message:'Invalid request'},e.status||400);}
  });
  server.requestTimeout=30000;server.headersTimeout=10000;server.on('close',()=>{clearInterval(timer);for(const t of transfers.values())stop(t);});
  return {server,transfers};
}
if(process.argv[1]===fileURLToPath(import.meta.url)){
  if(process.env.RELAY_REQUIRE_KEY==='true'&&!process.env.RELAY_REGISTRATION_KEY){console.error('RELAY_REGISTRATION_KEY is required');process.exit(1);}
  const relay=createRelay({stateFile:process.env.RELAY_STATE||path.resolve('work/relay/devices.json'),registrationKey:process.env.RELAY_REGISTRATION_KEY||''});
  relay.server.listen(Number(process.env.PORT||8787),process.env.HOST||'0.0.0.0',()=>console.log('Hearfolio relay listening on port '+relay.server.address().port+' (LAN HTTP test mode)'));
}
