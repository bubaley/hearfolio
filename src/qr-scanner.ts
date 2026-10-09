import {scan,cancel,checkPermissions,requestPermissions,Format} from '@tauri-apps/plugin-barcode-scanner';
import {icon} from './icons';
import {currentLanguage} from './i18n';
const t=(ru:string,en:string)=>currentLanguage()==='ru'?ru:en;
let active=false;
export async function scanInvitation():Promise<string|null>{
  if(active)return null;
  active=true;
  let overlay:HTMLDivElement|null=null,stop:()=>void=()=>{},cancelled=false;
  const app=document.querySelector<HTMLElement>('#app'),wasInert=app?.inert||false;
  const onKey=(e:KeyboardEvent)=>{if(e.key==='Escape'){e.preventDefault();stop();}};
  const onHide=()=>{if(document.hidden)stop();};
  try{
    let permission=await checkPermissions();
    if(permission!=='granted')permission=await requestPermissions();
    if(permission!=='granted')throw new Error('errors.cameraPermissionDenied');
    overlay=document.createElement('div');overlay.className='qr-scan-screen';overlay.setAttribute('role','dialog');overlay.setAttribute('aria-modal','true');overlay.setAttribute('aria-labelledby','qr-scan-title');
    overlay.innerHTML=`<div class="qr-scan-heading"><h2 id="qr-scan-title">${t('Сканировать QR','Scan QR')}</h2><button type="button" class="icon-button" aria-label="${t('Закрыть','Close')}">${icon('close')}</button></div><div class="qr-scan-frame" aria-hidden="true"></div><p>${t('Наведите камеру на QR-код в Hearfolio на другом устройстве.','Point the camera at the QR code in Hearfolio on your other device.')}</p>`;
    const stopped=new Promise<null>(resolve=>{stop=()=>{cancelled=true;resolve(null);};});
    overlay.querySelector('button')!.addEventListener('click',stop);
    document.body.append(overlay);document.documentElement.classList.add('qr-scanning');if(app)app.inert=true;
    document.addEventListener('keydown',onKey);document.addEventListener('visibilitychange',onHide);overlay.querySelector('button')!.focus();
    const result=await Promise.race([scan({windowed:true,cameraDirection:'back',formats:[Format.QRCode]}).then(r=>r.content),stopped]);
    return result;
  }catch(e){
    if(cancelled||String(e).toLowerCase().includes('cancelled'))return null;
    if(e instanceof Error&&e.message==='errors.cameraPermissionDenied')throw e;
    throw new Error('errors.qrScanFailed');
  }finally{
    if(overlay)await cancel().catch(()=>{});
    document.documentElement.classList.remove('qr-scanning');overlay?.remove();if(app)app.inert=wasInert;
    document.removeEventListener('keydown',onKey);document.removeEventListener('visibilitychange',onHide);active=false;
  }
}
