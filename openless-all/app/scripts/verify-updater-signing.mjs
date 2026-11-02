#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { verifyUpdaterArtifact } from './updater-signature.mjs';

const config = JSON.parse(readFileSync(new URL('../src-tauri/tauri.conf.json', import.meta.url)));
if (!process.env.TAURI_SIGNING_PRIVATE_KEY && !process.env.TAURI_SIGNING_PRIVATE_KEY_PATH) {
  console.error(
    'Updater signing requires TAURI_SIGNING_PRIVATE_KEY (not an Apple P12 certificate).',
  );
  process.exit(1);
}

const directory = mkdtempSync(join(tmpdir(), 'openless-updater-signing-'));
try {
  const challenge = join(directory, 'challenge');
  writeFileSync(challenge, 'OpenLess updater signing preflight\n');
  const cli = fileURLToPath(new URL('../node_modules/@tauri-apps/cli/tauri.js', import.meta.url));
  try {
    const env = {
      ...process.env,
      CI: 'true',
      TAURI_SIGNING_PRIVATE_KEY_PASSWORD: process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? '',
    };
    if (!env.TAURI_SIGNING_PRIVATE_KEY_PATH) delete env.TAURI_SIGNING_PRIVATE_KEY_PATH;
    execFileSync(
      process.execPath,
      [cli, 'signer', 'sign', challenge, '--app-version', config.version],
      {
        env,
        // Never forward signer output/errors: they may contain private key material.
        stdio: 'pipe',
        timeout: 30_000,
      },
    );
  } catch {
    throw new Error('Cannot sign updater challenge. Check the private key and its password.');
  }
  await verifyUpdaterArtifact(
    challenge,
    readFileSync(`${challenge}.sig`, 'utf8'),
    config.plugins.updater.pubkey,
    config.version,
  );
  console.log('Updater signing preflight passed: private key matches the embedded public key.');
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
} finally {
  rmSync(directory, { recursive: true, force: true });
}
