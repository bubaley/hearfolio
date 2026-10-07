// Generated Android projects are disposable. Apply tracked branding after init.
import {cp, mkdir, readFile, writeFile} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const app=path.resolve(process.argv[2]||path.join(root,'src-tauri/gen/android/app/src/main'));
const resources=path.join(app,'res');
const manifestPath=path.join(app,'AndroidManifest.xml');
let manifest=await readFile(manifestPath,'utf8');
if(!manifest.includes('android.intent.action.MAIN')||!manifest.includes('android.intent.category.LAUNCHER'))throw new Error('Android manifest must expose the launcher activity');
manifest=manifest.replace(/<application\b[^>]*>/s,tag=>{
  for(const [key,value] of [['icon','@mipmap/ic_launcher'],['roundIcon','@mipmap/ic_launcher_round']]){
    const pattern=new RegExp(`android:${key}="[^"]*"`);
    tag=pattern.test(tag)?tag.replace(pattern,`android:${key}="${value}"`):tag.replace('>',` android:${key}="${value}">`);
  }
  return tag;
});
await mkdir(resources,{recursive:true});
await cp(path.join(root,'src-tauri/icons/android'),resources,{recursive:true});
await writeFile(manifestPath,manifest);
console.log('Android launcher branding prepared');
