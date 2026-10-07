import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, generateKeyPairSync, randomBytes, sign } from 'node:crypto';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { bumpVersion, checkVersions, parseCommit, planRelease, releaseType, syncVersions } from './release.mjs';
import { assetName, createManifests, stageDesktop, verifyUpdaterSignature } from './release-artifacts.mjs';

function temporary(t) {
  const directory = mkdtempSync(join(tmpdir(), 'hearfolio-release-test-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}
function fixture(root) {
  mkdirSync(join(root, 'src-tauri'), { recursive: true });
  writeFileSync(join(root, 'package.json'), JSON.stringify({ name: 'speechdesk', version: '0.1.0' }));
  writeFileSync(join(root, 'package-lock.json'), JSON.stringify({ version: '0.1.0', packages: { '': { name: 'speechdesk', version: '0.1.0' }, 'node_modules/other': { version: '8.9.0' } } }));
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ version: '0.1.0', plugins: { updater: { pubkey: 'fixture' } } }));
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), '[package]\nname = "speechdesk"\nversion = "0.1.0"\n\n[dependencies]\nother = "8.9.0"\n');
  writeFileSync(join(root, 'src-tauri/Cargo.lock'), 'version = 4\n\n[[package]]\nname = "other"\nversion = "8.9.0"\n\n[[package]]\nname = "speechdesk"\nversion = "0.1.0"\ndependencies = ["other"]\n');
  writeFileSync(join(root, 'CHANGELOG.md'), '# Changelog\n\nInitial baseline.\n');
}
function git(root, ...args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}
function init(root) {
  git(root, 'init', '-b', 'main');
  git(root, 'config', 'user.name', 'Release Test');
  git(root, 'config', 'user.email', 'release-test@example.invalid');
}
function commit(root, message) { git(root, 'add', '.'); git(root, 'commit', '--allow-empty', '-m', message); }

test('Conventional Commit priority, breaking footers, and nonreleasing types', () => {
  assert.equal(releaseType(['fix(storage): preserve archive', 'feat: add Android']), 'minor');
  assert.equal(releaseType(['feat!: change archive', 'fix: recover files']), 'major');
  assert.equal(releaseType(['chore: rewrite protocol\n\nBREAKING CHANGE: old clients need migration']), 'major');
  assert.equal(releaseType(['fix: correct locale\n\nBREAKING-CHANGE: replacement API']), 'major');
  assert.equal(releaseType(['docs: update setup', 'ci: check binaries', 'refactor: simplify parser']), null);
  assert.equal(releaseType(['perf: reduce upload copies']), 'patch');
  assert.equal(releaseType(['revert: restore file import']), 'patch');
  assert.equal(releaseType(['Merge pull request #1 from branch', 'fix: retry']), 'patch');
  assert.throws(() => releaseType(['feat!: break API', 'invalid later commit']), /Conventional Commit/);
  assert.throws(() => parseCommit('fixed something'), /Conventional Commit/);
  assert.throws(() => parseCommit('feat: '), /Conventional Commit/);
  assert.equal(parseCommit('feat(updater): check desktop').scope, 'updater');
  assert.equal(bumpVersion('0.2.9', 'major'), '1.0.0');
  assert.equal(bumpVersion('1.2.9', 'minor'), '1.3.0');
  assert.equal(bumpVersion('1.2.9', 'patch'), '1.2.10');
  assert.throws(() => bumpVersion('01.2.0', 'patch'), /SemVer/);
});

test('Version synchronization preserves unrelated dependencies and updater config', t => {
  const root = temporary(t);
  fixture(root);
  syncVersions(root, '1.0.0');
  assert.equal(checkVersions(root), '1.0.0');
  assert.match(readFileSync(join(root, 'src-tauri/Cargo.lock'), 'utf8'), /name = "other"\nversion = "8.9.0"/);
  assert.match(readFileSync(join(root, 'src-tauri/Cargo.toml'), 'utf8'), /other = "8.9.0"/);
  assert.equal(JSON.parse(readFileSync(join(root, 'package-lock.json'))).packages['node_modules/other'].version, '8.9.0');
  assert.equal(JSON.parse(readFileSync(join(root, 'src-tauri/tauri.conf.json'))).plugins.updater.pubkey, 'fixture');
  const lock = JSON.parse(readFileSync(join(root, 'package-lock.json')));
  lock.packages[''].version = '0.1.0';
  writeFileSync(join(root, 'package-lock.json'), JSON.stringify(lock));
  assert.throws(() => checkVersions(root), /versions must agree/);
});

test('Prepare creates one immutable version commit and atomically pushes tag; next run is a no-op', t => {
  const root = temporary(t);
  const repository = join(root, 'source');
  const remote = join(root, 'remote.git');
  mkdirSync(repository);
  fixture(repository);
  init(repository);
  execFileSync('git', ['init', '--bare', remote], { stdio: 'ignore' });
  git(repository, 'remote', 'add', 'origin', remote);
  commit(repository, 'feat(app): initial application');
  git(repository, 'push', '-u', 'origin', 'main');
  assert.equal(planRelease(repository).version, '0.2.0');
  const script = resolve(dirname(fileURLToPath(import.meta.url)), 'release.mjs');
  const output = join(root, 'output');
  execFileSync(process.execPath, [script, 'prepare'], {
    cwd: repository,
    env: { ...process.env, GITHUB_SHA: git(repository, 'rev-parse', 'HEAD'), GITHUB_REPOSITORY: 'example/hearfolio', GITHUB_OUTPUT: output },
    stdio: 'pipe',
  });
  assert.equal(checkVersions(repository), '0.2.0');
  assert.equal(git(repository, 'rev-parse', 'HEAD'), git(repository, 'rev-parse', 'v0.2.0'));
  assert.equal(git(remote, 'rev-parse', 'main'), git(remote, 'rev-parse', 'v0.2.0'));
  assert.match(readFileSync(output, 'utf8'), /released=true\ntag=v0\.2\.0/);
  assert.match(readFileSync(join(repository, 'CHANGELOG.md'), 'utf8'), /## 0\.2\.0/);
  assert.equal(planRelease(repository).version, null);
  commit(repository, 'fix: preserve recording');
  assert.equal(planRelease(repository).version, '0.2.1');
  syncVersions(repository, '0.3.0');
  assert.throws(() => planRelease(repository), /differs from last release/);
});

test('Desktop staging fails on missing signatures or ambiguous build products', t => {
  const root = temporary(t);
  const bundle = join(root, 'bundle');
  const output = join(root, 'assets');
  mkdirSync(bundle);
  for (const extension of ['.AppImage', '.deb', '.rpm']) writeFileSync(join(bundle, `app${extension}`), 'binary');
  assert.throws(() => stageDesktop('0.2.0', 'linux_x86_64', bundle, output), /\.sig/);
  writeFileSync(join(bundle, 'app.AppImage.sig'), 'signature');
  writeFileSync(join(bundle, 'app.deb.sig'), 'signature');
  writeFileSync(join(bundle, 'app.rpm.sig'), 'signature');
  stageDesktop('0.2.0', 'linux_x86_64', bundle, output);
  assert.equal(readFileSync(join(output, assetName('0.2.0', 'linux_x86_64', '.deb')), 'utf8'), 'binary');
  writeFileSync(join(bundle, 'stale.deb'), 'binary');
  assert.throws(() => stageDesktop('0.2.0', 'linux_x86_64', bundle, output), /found 2/);
});

test('Windows staging requires one NSIS installer and its signature, ignoring application executables', t => {
  const root = temporary(t);
  const bundle = join(root, 'bundle');
  const output = join(root, 'assets');
  mkdirSync(join(bundle, 'nsis'), { recursive: true });
  writeFileSync(join(bundle, 'speechdesk.exe'), 'application executable');
  writeFileSync(join(bundle, 'nsis/Hearfolio_0.2.0_x64-setup.exe'), 'installer');
  assert.throws(() => stageDesktop('0.2.0', 'windows_x86_64', bundle, output), /\.sig/);
  writeFileSync(join(bundle, 'nsis/Hearfolio_0.2.0_x64-setup.exe.sig'), 'signature');
  stageDesktop('0.2.0', 'windows_x86_64', bundle, output);
  assert.equal(readFileSync(join(output, assetName('0.2.0', 'windows_x86_64', '-setup.exe')), 'utf8'), 'installer');
  writeFileSync(join(bundle, 'nsis/stale-setup.exe'), 'installer');
  assert.throws(() => stageDesktop('0.2.0', 'windows_x86_64', bundle, output), /found 2/);
});

test('Complete manifest maps all desktop targets, embeds signatures, hashes assets, rejects omissions', t => {
  const root = temporary(t);
  const version = '0.2.0';
  for (const [platform, extensions] of Object.entries({
    darwin_universal: ['.dmg', '.app.tar.gz', '.app.tar.gz.sig'],
    linux_x86_64: ['.AppImage', '.AppImage.sig', '.deb', '.deb.sig', '.rpm', '.rpm.sig'],
    windows_x86_64: ['-setup.exe', '-setup.exe.sig'],
    android_aarch64: ['.apk', '.aab'],
  })) for (const extension of extensions) {
    // Only metadata/schema is under test; real signatures are produced by Tauri.
    writeFileSync(join(root, assetName(version, platform, extension)), extension.endsWith('.sig') ? 'A'.repeat(100) : 'binary');
  }
  const aab = join(root, assetName(version, 'android_aarch64', '.aab'));
  rmSync(aab);
  assert.throws(() => createManifests(version, 'example/hearfolio', root, 'Notes'), /ENOENT/);
  writeFileSync(aab, 'binary');
  const windowsSignature = join(root, assetName(version, 'windows_x86_64', '-setup.exe.sig'));
  rmSync(windowsSignature);
  assert.throws(() => createManifests(version, 'example/hearfolio', root, 'Notes'), /ENOENT/);
  writeFileSync(windowsSignature, 'A'.repeat(100));
  const v4Sidecar = join(root, `${assetName(version, 'android_aarch64', '.apk')}.idsig`);
  writeFileSync(v4Sidecar, 'optional Android V4 signature');
  writeFileSync(join(root, 'unexpected.txt'), 'unknown file');
  assert.throws(() => createManifests(version, 'example/hearfolio', root, 'Notes'), /Unexpected release assets: unexpected\.txt/);
  assert.equal(existsSync(v4Sidecar), false);
  rmSync(join(root, 'unexpected.txt'));
  const result = createManifests(version, 'example/hearfolio', root, 'Notes', '2026-10-07T00:00:00Z');
  assert.deepEqual(Object.keys(result.manifest.platforms), ['darwin-aarch64', 'darwin-x86_64', 'linux-x86_64', 'linux-x86_64-appimage', 'linux-x86_64-deb', 'linux-x86_64-rpm', 'windows-x86_64', 'windows-x86_64-nsis']);
  assert.deepEqual(result.manifest.platforms['darwin-aarch64'], result.manifest.platforms['darwin-x86_64']);
  assert.match(result.manifest.platforms['linux-x86_64'].url, /releases\/download\/v0\.2\.0\//);
  assert.equal(result.manifest.platforms['linux-x86_64'].signature, 'A'.repeat(100));
  assert.match(result.manifest.platforms['linux-x86_64-deb'].url, /linux_x86_64\.deb$/);
  assert.match(result.manifest.platforms['linux-x86_64-rpm'].url, /linux_x86_64\.rpm$/);
  assert.match(result.manifest.platforms['windows-x86_64'].url, /windows_x86_64-setup\.exe$/);
  assert.deepEqual(result.manifest.platforms['windows-x86_64'], result.manifest.platforms['windows-x86_64-nsis']);
  assert.match(result.android.sha256, /^[a-f0-9]{64}$/);
  assert.match(readFileSync(join(root, 'SHA256SUMS.txt'), 'utf8'), /latest-android\.json/);
  const windowsInstaller = assetName(version, 'windows_x86_64', '-setup.exe');
  const binaryHash = createHash('sha256').update('binary').digest('hex');
  assert.match(readFileSync(join(root, 'SHA256SUMS.txt'), 'utf8'), new RegExp(`${binaryHash}  ${windowsInstaller.replaceAll('.', '\\.')}\\n`));
  assert.equal(result.assets.length, 16);
});

test('Minisign verifies the artifact, configured key, and trusted comment; tampering fails', () => {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const keyId = randomBytes(8);
  const key = Buffer.concat([Buffer.from('Ed'), keyId, publicKey.export({ format: 'der', type: 'spki' }).subarray(-32)]);
  const publicText = Buffer.from(`untrusted comment: minisign public key\n${key.toString('base64')}\n`).toString('base64');
  const bytes = Buffer.from('signed updater fixture');
  const signature = sign(null, createHash('blake2b512').update(bytes).digest(), privateKey);
  const signatureBytes = Buffer.concat([Buffer.from('ED'), keyId, signature]);
  const comment = 'timestamp:123\tfile:app.AppImage';
  const globalSignature = sign(null, Buffer.concat([signature, Buffer.from(comment)]), privateKey);
  const text = `untrusted comment: signature\n${signatureBytes.toString('base64')}\ntrusted comment: ${comment}\n${globalSignature.toString('base64')}\n`;
  const encoded = Buffer.from(text).toString('base64');
  verifyUpdaterSignature(bytes, encoded, publicText);
  assert.throws(() => verifyUpdaterSignature(Buffer.from('tampered'), encoded, publicText), /artifact signature/);
  assert.throws(() => verifyUpdaterSignature(bytes, Buffer.from(text.replace('timestamp:123', 'timestamp:124')).toString('base64'), publicText), /trusted comment/);
  key[2] ^= 1;
  const wrongKey = Buffer.from(`untrusted comment: minisign public key\n${key.toString('base64')}\n`).toString('base64');
  assert.throws(() => verifyUpdaterSignature(bytes, encoded, wrongKey), /does not match/);
});

test('Publisher verifies all five desktop signatures and refuses a tampered Windows installer', t => {
  const root = temporary(t);
  const assets = join(root, 'assets');
  mkdirSync(assets);
  mkdirSync(join(root, 'src-tauri'));
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const keyId = randomBytes(8);
  const key = Buffer.concat([Buffer.from('Ed'), keyId, publicKey.export({ format: 'der', type: 'spki' }).subarray(-32)]);
  const pubkey = Buffer.from(`untrusted comment: fixture key\n${key.toString('base64')}\n`).toString('base64');
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ plugins: { updater: { pubkey } } }));
  const signed = [
    ['darwin_universal', '.app.tar.gz'],
    ['linux_x86_64', '.AppImage'],
    ['linux_x86_64', '.deb'],
    ['linux_x86_64', '.rpm'],
    ['windows_x86_64', '-setup.exe'],
  ];
  for (const [platform, extension] of signed) {
    const name = assetName('0.2.0', platform, extension);
    const bytes = Buffer.from(`installer ${name}`);
    const signature = sign(null, createHash('blake2b512').update(bytes).digest(), privateKey);
    const comment = `file:${name}`;
    const signatureBytes = Buffer.concat([Buffer.from('ED'), keyId, signature]);
    const globalSignature = sign(null, Buffer.concat([signature, Buffer.from(comment)]), privateKey);
    const text = `untrusted comment: fixture signature\n${signatureBytes.toString('base64')}\ntrusted comment: ${comment}\n${globalSignature.toString('base64')}\n`;
    writeFileSync(join(assets, name), bytes);
    writeFileSync(join(assets, `${name}.sig`), Buffer.from(text).toString('base64'));
  }
  for (const [platform, extension] of [['darwin_universal', '.dmg'], ['android_aarch64', '.apk'], ['android_aarch64', '.aab']]) {
    writeFileSync(join(assets, assetName('0.2.0', platform, extension)), 'installer');
  }
  const notes = join(root, 'notes.md');
  writeFileSync(notes, 'Notes');
  const script = resolve(dirname(fileURLToPath(import.meta.url)), 'release-artifacts.mjs');
  const run = () => execFileSync(process.execPath, [script, 'manifest', '0.2.0', 'example/hearfolio', assets, notes], { cwd: root, encoding: 'utf8', stdio: 'pipe' });
  assert.match(run(), /Validated 16 release assets/);
  writeFileSync(join(assets, assetName('0.2.0', 'windows_x86_64', '-setup.exe')), 'tampered installer');
  assert.throws(run, error => error.status === 1 && /artifact signature verification failed/.test(error.stderr));
});
