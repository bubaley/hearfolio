export type TransferPreferences={autoPair:boolean;autoReceive:boolean};
type Inbox={pairRequests:{id:string}[];offers:{id:string}[]};
// A failed automatic action stays visible for manual retry instead of looping.
export function automaticActions(inbox:Inbox,preferences:TransferPreferences,busy:boolean,attempted:ReadonlySet<string>){
  return {
    pair:preferences.autoPair?inbox.pairRequests.filter(p=>!attempted.has(`pair:${p.id}`)).map(p=>p.id):[],
    receive:preferences.autoReceive&&!busy?inbox.offers.find(p=>!attempted.has(`receive:${p.id}`))?.id:null,
  };
}
