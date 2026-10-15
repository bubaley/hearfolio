export type TransferPreferences={autoPair:boolean;autoReceive:boolean};
type Inbox={pairRequests:{id:string}[];offers:{id:string}[]};
// A failed automatic action stays visible for manual retry instead of looping.
export function automaticActions(inbox:Inbox,preferences:TransferPreferences,busy:boolean,attempted:ReadonlySet<string>){
  return {
    pair:preferences.autoPair?inbox.pairRequests.filter(p=>!attempted.has(`pair:${p.id}`)).map(p=>p.id):[],
    receive:preferences.autoReceive&&!busy?inbox.offers.find(p=>!attempted.has(`receive:${p.id}`))?.id:null,
  };
}

// An address exists only when the user or a saved native connection provided it.
export function configuredServer(value:string|null|undefined){
  const address=value?.trim().replace(/\/+$/,'')||'';
  try{const url=new URL(address),host=url.hostname,ip=host.split('.').map(Number);
    const local=host==='localhost'||(ip.length===4&&ip.every(n=>Number.isInteger(n)&&n>=0&&n<=255)&&(ip[0]===127||ip[0]===10||(ip[0]===192&&ip[1]===168)||(ip[0]===172&&ip[1]>=16&&ip[1]<=31)));
    return url.protocol==='http:'&&local&&!url.username&&!url.password&&!url.search&&!url.hash&&url.pathname==='/'?address:'';
  }catch{return '';}
}
export function invitationSeconds(expiresAt:number,now:number){return Math.max(0,Math.ceil((expiresAt-now)/1000));}
