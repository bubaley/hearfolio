import { execFileSync } from 'node:child_process';
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const VERSION_FILES = ['package.json', 'package-lock.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'src-tauri/tauri.conf.json'];
const TYPES = 'feat|fix|perf|revert|docs|style|refactor|test|build|ci|chore';
const HEADER = new RegExp(`^(${TYPES})(?:\\(([^()\\r\\n]+)\\))?(!)?: (\\S.*)$`);
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

export function parseCommit(message) {
  const [subject] = message.split(/\r?\n/);
  const match = HEADER.exec(subject);
  if (!match) throw new Error(`Expected Conventional Commit: ${subject}`);
  return {
    type: match[1], scope: match[2], description: match[4], subject,
    breaking: Boolean(match[3]) || /^BREAKING(?: CHANGE|-CHANGE): \S/m.test(message),
  };
}

export function releaseType(messages) {
  let result = null;
  for (const message of messages) {
    // Git's own merge commits contain no change description. Squash PRs by default.
    if (/^Merge (?:pull request|branch|remote-tracking branch) /.test(message)) continue;
    const commit = parseCommit(message);
    if (commit.breaking) result = 'major';
    if (commit.type === 'feat' && result !== 'major') result = 'minor';
    else if (['fix', 'perf', 'revert'].includes(commit.type) && !result) result = 'patch';
  }
  return result;
}

export function bumpVersion(version, type) {
  if (!SEMVER.test(version)) throw new Error(`Invalid stable SemVer: ${version}`);
  const [major, minor, patch] = version.split('.').map(Number);
  if (type === 'major') return `${major + 1}.0.0`;
  if (type === 'minor') return `${major}.${minor + 1}.0`;
  if (type === 'patch') return `${major}.${minor}.${patch + 1}`;
  throw new Error(`Invalid release type: ${type}`);
}

function readJson(root, file) { return JSON.parse(readFileSync(resolve(root, file), 'utf8')); }
function packageBlock(text) {
  const block = text.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
  if (!block) throw new Error('Missing Cargo [package]');
  const name = block.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
  const version = block.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!name || !version) throw new Error('Cargo package name/version must be explicit');
  return { name, version };
}
function rootLockBlock(text, name) {
  const blocks = text.split('[[package]]');
  const matches = blocks.slice(1).filter(block => block.match(/^\s*name\s*=\s*"([^"]+)"/m)?.[1] === name);
  if (matches.length !== 1) throw new Error(`Expected one Cargo.lock package for ${name}`);
  return matches[0];
}

export function checkVersions(root = '.') {
  const pkg = readJson(root, 'package.json');
  const lock = readJson(root, 'package-lock.json');
  const config = readJson(root, 'src-tauri/tauri.conf.json');
  const cargo = packageBlock(readFileSync(resolve(root, 'src-tauri/Cargo.toml'), 'utf8'));
  const lockVersion = rootLockBlock(readFileSync(resolve(root, 'src-tauri/Cargo.lock'), 'utf8'), cargo.name).match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const versions = [pkg.version, lock.version, lock.packages?.['']?.version, config.version, cargo.version, lockVersion];
  if (!SEMVER.test(pkg.version) || versions.some(version => version !== pkg.version)) {
    throw new Error(`Application versions must agree: ${versions.join(', ')}`);
  }
  return pkg.version;
}

export function syncVersions(root, nextVersion) {
  if (!SEMVER.test(nextVersion)) throw new Error(`Invalid stable SemVer: ${nextVersion}`);
  checkVersions(root);
  for (const file of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']) {
    const value = readJson(root, file);
    value.version = nextVersion;
    if (file === 'package-lock.json') value.packages[''].version = nextVersion;
    writeFileSync(resolve(root, file), `${JSON.stringify(value, null, 2)}\n`);
  }
  const cargoFile = resolve(root, 'src-tauri/Cargo.toml');
  const cargoText = readFileSync(cargoFile, 'utf8');
  const cargo = packageBlock(cargoText);
  writeFileSync(cargoFile, cargoText.replace(/(^\[package\]\s*\n[\s\S]*?^version\s*=\s*")[^"]+(".*$)/m, (_, prefix, suffix) => `${prefix}${nextVersion}${suffix}`));
  const lockFile = resolve(root, 'src-tauri/Cargo.lock');
  const lockText = readFileSync(lockFile, 'utf8');
  const block = rootLockBlock(lockText, cargo.name);
  writeFileSync(lockFile, lockText.replace(block, block.replace(/(^version\s*=\s*")[^"]+(".*$)/m, (_, prefix, suffix) => `${prefix}${nextVersion}${suffix}`)));
  checkVersions(root);
}

function git(args, root = '.') { return execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim(); }
function commitsSince(tag, root = '.') {
  return git(['log', '--format=%H%x00%B%x00', ...(tag ? [`${tag}..HEAD`] : ['HEAD'])], root)
    .split('\0').reduce((commits, item, index, all) => {
      if (index % 2 === 0 && item.trim()) commits.push({ sha: item.trim(), message: all[index + 1].trim() });
      return commits;
    }, []);
}

export function planRelease(root = '.') {
  const current = checkVersions(root);
  const tag = latestTag(root);
  if (tag && current !== tag.slice(1)) throw new Error(`Version ${current} differs from last release ${tag}`);
  const commits = commitsSince(tag, root);
  const type = releaseType(commits.map(commit => commit.message));
  return { current, previousTag: tag ?? null, type, version: type ? bumpVersion(current, type) : null, commits };
}

function latestTag(root = '.', ref = 'HEAD') {
  return git(['tag', '--merged', ref, '--list', 'v*', '--sort=-version:refname'], root)
    .split('\n').find(candidate => SEMVER.test(candidate.slice(1)));
}

export function releaseNotes(version, commits, repository, date = new Date().toISOString().slice(0, 10)) {
  const items = commits.filter(commit => !/^Merge /.test(commit.message)).map(commit => ({ ...commit, ...parseCommit(commit.message) }));
  const groups = [
    ['Breaking changes', items.filter(item => item.breaking)],
    ['Features', items.filter(item => item.type === 'feat' && !item.breaking)],
    ['Fixes and performance', items.filter(item => ['fix', 'perf', 'revert'].includes(item.type) && !item.breaking)],
    ['Maintenance', items.filter(item => !['feat', 'fix', 'perf', 'revert'].includes(item.type) && !item.breaking)],
  ];
  return `## ${version} (${date})\n\n` + groups.filter(([, entries]) => entries.length).map(([title, entries]) =>
    `### ${title}\n\n${entries.map(item => `- ${item.scope ? `**${item.scope}:** ` : ''}${item.description} ([${item.sha.slice(0, 7)}](https://github.com/${repository}/commit/${item.sha}))`).join('\n')}\n`,
  ).join('\n');
}

function output(values) {
  if (process.env.GITHUB_OUTPUT) for (const [key, value] of Object.entries(values)) appendFileSync(process.env.GITHUB_OUTPUT, `${key}=${value}\n`);
  console.log(JSON.stringify(values));
}

function run() {
  const [command, argument] = process.argv.slice(2);
  if (command === 'check') return console.log(checkVersions());
  if (command === 'latest-tag') return console.log(latestTag('.', argument ?? 'HEAD') ?? '');
  if (command === 'lint-title') return console.log(parseCommit(process.env.PR_TITLE ?? argument ?? '').subject);
  if (command === 'lint-range') {
    releaseType(commitsSince(argument).map(commit => commit.message));
    return console.log('Conventional Commits validated');
  }
  if (command === 'plan') return console.log(JSON.stringify(planRelease(), null, 2));
  if (command === 'notes') {
    if (!/^v\d+\.\d+\.\d+$/.test(argument ?? '')) throw new Error('Expected vMAJOR.MINOR.PATCH tag');
    const tags = git(['tag', '--merged', `${argument}^`, '--list', 'v*', '--sort=-version:refname']).split('\n');
    const previous = tags.find(tag => SEMVER.test(tag.slice(1)));
    const commits = commitsSince(previous).filter(commit => !/^chore\(release\):/.test(commit.message));
    return writeFileSync('release-notes.md', releaseNotes(argument.slice(1), commits, process.env.GITHUB_REPOSITORY ?? 'bubaley/hearfolio'));
  }
  if (command === 'prepare') {
    if (git(['status', '--porcelain'])) throw new Error('Release preparation requires a clean checkout');
    if (git(['rev-parse', 'HEAD']) !== process.env.GITHUB_SHA) throw new Error('Refusing to release a commit different from the checked CI commit');
    const plan = planRelease();
    if (!plan.version) return output({ released: false });
    const repository = process.env.GITHUB_REPOSITORY ?? 'bubaley/hearfolio';
    const notes = releaseNotes(plan.version, plan.commits, repository);
    syncVersions('.', plan.version);
    const old = readFileSync('CHANGELOG.md', 'utf8');
    writeFileSync('CHANGELOG.md', old.replace('# Changelog\n', `# Changelog\n\n${notes}`));
    git(['add', '--', ...VERSION_FILES, 'CHANGELOG.md']);
    git(['-c', 'user.name=github-actions[bot]', '-c', 'user.email=41898282+github-actions[bot]@users.noreply.github.com', 'commit', '-m', `chore(release): ${plan.version} [skip ci]`]);
    const tag = `v${plan.version}`;
    git(['tag', tag]);
    // Never force push. A concurrent main update or branch rule rejection fails safely.
    git(['push', '--atomic', 'origin', 'HEAD:refs/heads/main', `refs/tags/${tag}`]);
    return output({ released: true, tag, sha: git(['rev-parse', 'HEAD']), version: plan.version });
  }
  throw new Error('Usage: node scripts/release.mjs check|latest-tag [ref]|lint-title|lint-range <base>|plan|prepare|notes <tag>');
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { run(); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
