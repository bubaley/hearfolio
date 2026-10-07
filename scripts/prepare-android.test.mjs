import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,readFile,writeFile,rm} from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {tmpdir} from 'node:os';
import path from 'node:path';
test('generated Android branding survives repeated preparation',async()=>{
  const directory=await mkdtemp(path.join(tmpdir(),'hearfolio-android-icons-'));
  try{
    const manifest=path.join(directory,'AndroidManifest.xml');
    await writeFile(manifest,'<manifest><application android:icon="@mipmap/default"><activity><intent-filter><action android:name="android.intent.action.MAIN"/><category android:name="android.intent.category.LAUNCHER"/></intent-filter></activity></application></manifest>');
    for(let run=0;run<2;run++)execFileSync(process.execPath,['scripts/prepare-android.mjs',directory]);
    const source=await readFile(manifest,'utf8');
    assert.equal(source.match(/android:icon=/g)?.length,1);
    assert.equal(source.match(/android:roundIcon=/g)?.length,1);
    assert.ok(source.includes('@mipmap/ic_launcher'));
    assert.deepEqual(await readFile(path.join(directory,'res/mipmap-xxxhdpi/ic_launcher.png')),await readFile('src-tauri/icons/android/mipmap-xxxhdpi/ic_launcher.png'));
    assert.deepEqual(await readFile(path.join(directory,'res/mipmap-anydpi-v26/ic_launcher.xml')),await readFile('src-tauri/icons/android/mipmap-anydpi-v26/ic_launcher.xml'));
    await writeFile(manifest,'<manifest><application/></manifest>');
    assert.throws(()=>execFileSync(process.execPath,['scripts/prepare-android.mjs',directory],{stdio:'pipe'}),/launcher/);
  }finally{await rm(directory,{recursive:true,force:true});}
});
