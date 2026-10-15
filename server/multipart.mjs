import {createHash} from 'node:crypto';

export const PART_SIZE=1024*1024, PART_WINDOW=4;
const fail=(message,status=400)=>{throw Object.assign(new Error(message),{status});};
export function createParts(description,size) {
  if(description===undefined)return null;
  if(!description||description.size!==PART_SIZE||!Array.isArray(description.hashes)||description.hashes.length!==Math.ceil(size/PART_SIZE)||!description.hashes.every(h=>typeof h==='string'&&/^[a-f0-9]{64}$/.test(h)))fail('Invalid part manifest');
  return {size:PART_SIZE,hashes:description.hashes,window:PART_WINDOW,enabled:false,acked:new Set(),pending:new Map()};
}
export function describeParts(t) {return t.parts?{size:t.parts.size,hashes:t.parts.hashes,window:PART_WINDOW}:null;}
export function partLength(t,index) {return Math.min(PART_SIZE,t.manifest.size-index*PART_SIZE);}
export function partIndex(t,value) {
  if(!/^(0|[1-9]\d*)$/.test(String(value)))fail('Invalid part index');
  const index=Number(value);if(!Number.isSafeInteger(index)||index>=t.parts.hashes.length)fail('Invalid part index');return index;
}
export function acceptParts(t,description) {
  if(!t.parts||!description||!Array.isArray(description.verified)||description.verified.length>t.parts.hashes.length)fail('Invalid resume state');
  const verified=description.verified.map(v=>partIndex(t,v));
  if(new Set(verified).size!==verified.length)fail('Duplicate verified part');
  t.parts.enabled=true;t.parts.acked=new Set(verified);
  t.offset=verified.reduce((n,i)=>n+partLength(t,i),0);
}
export function acknowledgePart(t,value,hash,json) {
  const index=partIndex(t,value);
  if(hash!==t.parts.hashes[index])fail('Part checksum mismatch',422);
  if(t.parts.acked.has(index))return;
  const part=t.parts.pending.get(index);if(!part?.buffer)fail('Part not available',409);
  t.parts.acked.add(index);t.parts.pending.delete(index);t.offset+=partLength(t,index);
  if(part.res)json(part.res,{index,offset:t.offset});
}
export function stopParts(t,json) {
  if(!t.parts)return;
  for(const p of t.parts.pending.values())if(p.res)json(p.res,{error:'Transfer cancelled'},409);
  t.parts.pending.clear();
}
export async function handlePart(req,res,t,value,device,read,json,transfers) {
  if(!t.parts?.enabled||t.status!=='streaming'||!t.audioRequired)fail('Multipart transfer is not active',409);
  const index=partIndex(t,value);
  if(req.method==='PUT'&&device.id===t.from) {
    if(req.headers['x-part-sha256']!==t.parts.hashes[index])fail('Part checksum mismatch',422);
    if(t.parts.acked.has(index)){req.resume();return json(res,{index,offset:t.offset});}
    const existing=t.parts.pending.get(index);
    if(existing){req.resume();if(existing.res||!existing.buffer)fail('Part already in flight',429);existing.res=res;res.on('close',()=>{if(existing.res===res)existing.res=null;});return;}
    const active=[...transfers.values()].filter(x=>x.parts?.enabled);
    const slots=active.reduce((n,x)=>n+x.parts.pending.size,0);
    const owned=Math.max(...[t.from,t.to].map(id=>active.filter(x=>x.from===id||x.to===id).reduce((n,x)=>n+x.parts.pending.size,0)));
    if(slots>=32||owned>=PART_WINDOW||t.parts.pending.size>=PART_WINDOW){req.resume();fail('Part window full',429);}
    // Reserve before reading: simultaneous uploads cannot exceed the window.
    const part={buffer:null,res};t.parts.pending.set(index,part);
    res.on('close',()=>{if(part.res===res)part.res=null;});
    try {
      const buffer=await read(req,partLength(t,index));
      if(buffer.length!==partLength(t,index)||createHash('sha256').update(buffer).digest('hex')!==t.parts.hashes[index])fail('Part checksum mismatch',422);
      if(t.status!=='streaming'||t.parts.pending.get(index)!==part)fail('Transfer changed',409);
      part.buffer=buffer;
    } catch(e){if(t.parts.pending.get(index)===part)t.parts.pending.delete(index);throw e;}
    return;
  }
  if(req.method==='GET'&&device.id===t.to) {
    const part=t.parts.pending.get(index);
    if(!part?.buffer){res.writeHead(204);return res.end();}
    res.writeHead(200,{'content-type':'application/octet-stream','x-part-sha256':t.parts.hashes[index],'content-length':part.buffer.length,'cache-control':'no-store'});return res.end(part.buffer);
  }
  fail('Not found',404);
}
