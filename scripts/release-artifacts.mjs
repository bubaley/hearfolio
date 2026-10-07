import { createHash, createPublicKey, verify } from 'node:crypto';
import { copyFileSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const DESKTOP = {
  darwin_universal: ['.dmg', '.app.tar.gz', '.app.tar.gz.sig'],
  linux_x86_64: ['.AppImage', '.AppImage.sig', '.deb', '.deb.sig', '.rpm', '.rpm.sig'],
};
export function assetName(version, platform, extension) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) throw new Error('Invalid stable release version');
  return `Hearfolio_${version}_${platform}${extension}`;
}
function filesUnder(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return filesUnder(path);
    return entry.isFile() ? [path] : [];
  });
}
export function stageDesktop(version, platform, directory, output) {
  const extensions = DESKTOP[platform];
  if (!extensions) throw new Error(`Unsupported desktop platform: ${platform}`);
  const files = filesUnder(directory);
  mkdirSync(output, { recursive: true });
  for (const extension of extensions) {
    const matches = files.filter(path => path.endsWith(extension));
    if (matches.length !== 1 || statSync(matches[0]).size === 0) throw new Error(`Expected exactly one nonempty ${extension}; found ${matches.length}`);
    copyFileSync(matches[0], join(output, assetName(version, platform, extension)));
  }
}
function sha256(path) { return createHash('sha256').update(readFileSync(path)).digest('hex'); }

// Tauri wraps standard Minisign text files in base64. Verify both the artifact
// and the trusted comment with Node's Ed25519 implementation before publication.
export function verifyUpdaterSignature(bytes, encodedSignature, encodedPublicKey) {
  const publicText = Buffer.from(encodedPublicKey.trim(), 'base64').toString('utf8').trim().split(/\r?\n/);
  const signatureText = Buffer.from(encodedSignature.trim(), 'base64').toString('utf8').trim().split(/\r?\n/);
  if (publicText.length !== 2 || signatureText.length !== 4 || !publicText[0].startsWith('untrusted comment:') || !signatureText[0].startsWith('untrusted comment:') || !signatureText[2].startsWith('trusted comment: ')) throw new Error('Invalid Tauri Minisign text');
  const publicBytes = Buffer.from(publicText[1], 'base64');
  const signatureBytes = Buffer.from(signatureText[1], 'base64');
  const globalSignature = Buffer.from(signatureText[3], 'base64');
  if (publicBytes.length !== 42 || signatureBytes.length !== 74 || globalSignature.length !== 64 || publicBytes.subarray(0, 2).toString() !== 'Ed') throw new Error('Invalid Minisign key or signature');
  if (!publicBytes.subarray(2, 10).equals(signatureBytes.subarray(2, 10))) throw new Error('Updater signing key does not match the configured public key');
  const algorithm = signatureBytes.subarray(0, 2).toString();
  if (!['ED', 'Ed'].includes(algorithm)) throw new Error('Unsupported Minisign signature algorithm');
  const publicKey = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), publicBytes.subarray(10)]), format: 'der', type: 'spki' });
  const signedBytes = algorithm === 'ED' ? createHash('blake2b512').update(bytes).digest() : bytes;
  const signature = signatureBytes.subarray(10);
  if (!verify(null, signedBytes, publicKey, signature)) throw new Error('Updater artifact signature verification failed');
  const trustedComment = Buffer.from(signatureText[2].slice('trusted comment: '.length));
  if (!verify(null, Buffer.concat([signature, trustedComment]), publicKey, globalSignature)) throw new Error('Updater trusted comment verification failed');
}

export function verifyUpdaterAssets(directory, publicKey) {
  for (const signatureFile of readdirSync(directory).filter(file => file.endsWith('.sig'))) {
    verifyUpdaterSignature(readFileSync(join(directory, signatureFile.slice(0, -4))), readFileSync(join(directory, signatureFile), 'utf8'), publicKey);
  }
}
export function createManifests(version, repository, directory, notes, date = new Date().toISOString()) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error('Invalid GitHub repository');
  const required = Object.entries(DESKTOP).flatMap(([platform, extensions]) => extensions.map(extension => assetName(version, platform, extension)));
  required.push(assetName(version, 'android_aarch64', '.apk'), assetName(version, 'android_aarch64', '.aab'));
  for (const file of required) {
    const path = join(directory, file);
    if (!statSync(path).isFile() || statSync(path).size === 0) throw new Error(`Missing or empty release asset: ${file}`);
  }
  // apksigner enables its optional V4 sidecar by default. It is only needed for
  // incremental ADB installation, not normal installation of the signed APK.
  // Older tagged builds may include this one proven extra file; remove only it.
  const androidV4Sidecar = join(directory, `${assetName(version, 'android_aarch64', '.apk')}.idsig`);
  if (existsSync(androidV4Sidecar)) {
    if (!lstatSync(androidV4Sidecar).isFile()) throw new Error('Android V4 sidecar must be a regular file');
    rmSync(androidV4Sidecar);
  }
  const unexpected = readdirSync(directory).filter(file => !required.includes(file));
  if (unexpected.length) throw new Error(`Unexpected release assets: ${unexpected.join(', ')}`);
  const assetUrl = name => `https://github.com/${repository}/releases/download/v${version}/${encodeURIComponent(name)}`;
  const platforms = {};
  for (const [platform, extension, targets] of [
    ['darwin_universal', '.app.tar.gz', ['darwin-aarch64', 'darwin-x86_64']],
    ['linux_x86_64', '.AppImage', ['linux-x86_64', 'linux-x86_64-appimage']],
    ['linux_x86_64', '.deb', ['linux-x86_64-deb']],
    ['linux_x86_64', '.rpm', ['linux-x86_64-rpm']],
  ]) {
    const name = assetName(version, platform, extension);
    const signature = readFileSync(join(directory, `${name}.sig`), 'utf8').trim();
    if (!/^[A-Za-z0-9+/=]{64,}$/.test(signature)) throw new Error(`Invalid updater signature: ${name}.sig`);
    for (const target of targets) platforms[target] = { signature, url: assetUrl(name) };
  }
  const manifest = { version, notes, pub_date: date, platforms };
  const apk = assetName(version, 'android_aarch64', '.apk');
  const android = { version, notes, pub_date: date, url: assetUrl(apk), sha256: sha256(join(directory, apk)) };
  writeFileSync(join(directory, 'latest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  writeFileSync(join(directory, 'latest-android.json'), `${JSON.stringify(android, null, 2)}\n`);
  const sums = [...required, 'latest.json', 'latest-android.json'].sort().map(file => `${sha256(join(directory, file))}  ${file}`).join('\n');
  writeFileSync(join(directory, 'SHA256SUMS.txt'), `${sums}\n`);
  return { manifest, android, assets: [...required, 'latest.json', 'latest-android.json', 'SHA256SUMS.txt'] };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const [command, ...args] = process.argv.slice(2);
    if (command === 'stage') stageDesktop(...args);
    else if (command === 'manifest') {
      const [version, repository, directory, notesFile] = args;
      const config = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
      verifyUpdaterAssets(directory, config.plugins.updater.pubkey);
      const result = createManifests(version, repository, directory, readFileSync(notesFile, 'utf8'));
      console.log(`Validated ${result.assets.length} release assets`);
    } else throw new Error('Usage: release-artifacts.mjs stage <version> <platform> <bundle-dir> <output> | manifest <version> <owner/repo> <asset-dir> <notes-file>');
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
