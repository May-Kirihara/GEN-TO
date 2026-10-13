#!/usr/bin/env node
// WP-W-WORKER: embedding workerのビルドとTauri sidecar配置の自動化。
//
// 使い方:
//   node scripts/prepare-worker.mjs                       # releaseビルド + sidecar配置
//   node scripts/prepare-worker.mjs --dev                 # debugビルドのみ (コピーなし)
//   node scripts/prepare-worker.mjs --target <triple>     # host tripleを上書き (クロス用)
//
// tauri.conf.json の beforeBuildCommand / beforeDevCommand から呼ばれる。
// Tauri v2の externalBin 契約上、バンドルには
// apps/desktop/src-tauri/binaries/gento-embedding-worker-<target-triple>[.exe]
// というファイル名がビルド時に必要 (バンドル先でtriple接尾が落ち、main exe隣に
// 配置される → resolve_worker_path のexe隣接候補と一致)。
// --dev でコピーしないのは、dev実行が target/debug/ から直接行われるため
// (exe隣接候補がそのまま効く)。

import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const WORKER_NAME = 'gento-embedding-worker';
const SIDECAR_DIR = path.join(REPO_ROOT, 'apps/desktop/src-tauri/binaries');

function fail(message) {
  console.error(`[prepare-worker] ERROR: ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const parsed = { dev: false, target: null };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--dev') {
      parsed.dev = true;
    } else if (arg === '--target') {
      parsed.target = argv[++i];
      if (!parsed.target) fail('--target requires a value (e.g. --target x86_64-pc-windows-msvc)');
    } else {
      fail(`unknown argument: ${arg} (usable: --dev, --target <triple>)`);
    }
  }
  return parsed;
}

function run(command, args) {
  const result = spawnSync(command, args, { cwd: REPO_ROOT, stdio: 'inherit' });
  if (result.error) fail(`failed to start ${command}: ${result.error.message}`);
  if (result.status !== 0) {
    console.error(`[prepare-worker] ${command} ${args.join(' ')} exited with status ${result.status}`);
    process.exit(result.status ?? 1);
  }
}

function resolveTriple(override) {
  if (override) return override;
  const result = spawnSync('rustc', ['-vV'], { encoding: 'utf8' });
  if (result.status !== 0 || result.error) {
    fail('rustc -vV failed — Rust toolchain not found on PATH');
  }
  const match = /^host:\s*(\S+)$/m.exec(result.stdout);
  if (!match) fail('could not parse host triple from `rustc -vV` output');
  return match[1];
}

const { dev, target } = parseArgs(process.argv.slice(2));
const triple = resolveTriple(target);
const exeSuffix = process.platform === 'win32' || triple.includes('-windows-') ? '.exe' : '';

const buildArgs = ['build', '-p', 'gento-embedding', '--bin', WORKER_NAME];
if (!dev) buildArgs.push('--release');
if (target) buildArgs.push('--target', target);

console.log(`[prepare-worker] cargo build (${dev ? 'debug' : 'release'}) ${WORKER_NAME} [triple: ${triple}]`);
run('cargo', buildArgs);

if (dev) {
  console.log('[prepare-worker] dev mode: debug build done (コピーなし — dev実行はtarget/debug配下から解決)');
  process.exit(0);
}

const profileDir = target ? path.join('target', target, 'release') : path.join('target', 'release');
const built = path.join(REPO_ROOT, profileDir, `${WORKER_NAME}${exeSuffix}`);
if (!existsSync(built)) fail(`built worker not found: ${built}`);

mkdirSync(SIDECAR_DIR, { recursive: true });
const sidecar = path.join(SIDECAR_DIR, `${WORKER_NAME}-${triple}${exeSuffix}`);
copyFileSync(built, sidecar);
console.log(`[prepare-worker] sidecar copied: ${path.relative(REPO_ROOT, built)} -> ${path.relative(REPO_ROOT, sidecar)}`);
