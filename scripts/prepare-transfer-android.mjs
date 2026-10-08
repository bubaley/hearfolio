// Keep the local MVP on the installed SDK/NDK rather than downloading AGP defaults.
import './prepare-android.mjs';
import {readFile,writeFile} from 'node:fs/promises';
const path=new URL('../src-tauri/gen/android/app/build.gradle.kts',import.meta.url);
let gradle=await readFile(path,'utf8');
gradle=gradle.replace(/\n\s*buildToolsVersion\s*=\s*"[^"]*"/g,'').replace(/\n\s*ndkVersion\s*=\s*"[^"]*"/g,'');
gradle=gradle.replace('android {','android {\n    buildToolsVersion = "36.0.0"\n    ndkVersion = "27.2.12479018"');
await writeFile(path,gradle);
console.log('Transfer MVP SDK and NDK pinned');
