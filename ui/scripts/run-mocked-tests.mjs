#!/usr/bin/env node
// Build-and-run helper for the fully mocked Playwright specs.
//
// `npm run test:mocked` builds the UI, starts `vite preview` on :4173 and runs
// the `mocked` project against it. The specs answer every /api/ path with
// page.route, so no live node, Authelia session or fake credentials are needed.
// The preview process is always stopped, including on Ctrl-C.
import { spawn } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const bin = (name) => join(root, 'node_modules', '.bin', name);
const baseURL = 'http://localhost:4173';

function stop(child) {
  if (child && child.exitCode === null && !child.killed) child.kill('SIGTERM');
}

async function waitForServer(child) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`vite preview exited before becoming ready (code ${child.exitCode})`);
    }
    try {
      const res = await fetch(baseURL, { signal: AbortSignal.timeout(1_000) });
      if (res.ok) return;
    } catch {
      // Not listening yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`vite preview did not answer on ${baseURL} within 60s`);
}

async function main() {
  const preview = spawn(bin('vite'), ['preview', '--port', '4173', '--strictPort'], {
    cwd: root,
    stdio: 'inherit',
    env: process.env
  });
  const stopPreview = () => stop(preview);
  process.on('exit', stopPreview);
  process.on('SIGINT', () => {
    stopPreview();
    process.exit(130);
  });
  process.on('SIGTERM', () => {
    stopPreview();
    process.exit(143);
  });

  try {
    await waitForServer(preview);
    const tests = spawn(bin('playwright'), ['test', '--project=mocked'], {
      cwd: root,
      stdio: 'inherit',
      env: { ...process.env, PLAYWRIGHT_BASE_URL: baseURL }
    });
    const code = await new Promise((resolve, reject) => {
      tests.on('error', reject);
      tests.on('exit', (exitCode, signal) => resolve(exitCode ?? (signal ? 1 : 0)));
    });
    stopPreview();
    process.exit(code);
  } catch (error) {
    stopPreview();
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  }
}

void main();
