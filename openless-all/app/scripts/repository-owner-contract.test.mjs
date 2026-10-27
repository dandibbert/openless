import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = fileURLToPath(new URL('../../..', import.meta.url));
const currentRepo = 'dandibbert/openless';
const legacyRepo = 'appergb/openless';

const repositoryReferences = [
  '.github/ISSUE_TEMPLATE/config.yml',
  '.github/workflows/android-apk.yml',
  '.github/workflows/release-tauri.yml',
  'Casks/openless.rb',
  'README.md',
  'README.zh.md',
  'USAGE.md',
  'openless-all/app/src-tauri/src/android/updater_logic.rs',
  'openless-all/app/src-tauri/src/commands/mod.rs',
  'openless-all/app/src-tauri/src/commands/settings.rs',
  'openless-all/app/src-tauri/src/update_source.rs',
  'openless-all/app/src-tauri/tauri.conf.json',
  'openless-all/app/src/components/AutoUpdate.tsx',
  'openless-all/app/src/components/SettingsModal.tsx',
  'openless-all/app/src/pages/settings/AboutSection.tsx',
  'openless-all/app/scripts/write-updater-manifest.mjs',
];

for (const relativePath of repositoryReferences) {
  const content = await readFile(join(repoRoot, relativePath), 'utf8');
  assert(
    !content.includes(legacyRepo),
    `${relativePath} still references the pre-transfer repository`,
  );
}

const tauriConfig = JSON.parse(
  await readFile(join(repoRoot, 'openless-all/app/src-tauri/tauri.conf.json'), 'utf8'),
);
const updaterEndpoints = tauriConfig?.plugins?.updater?.endpoints;
assert(
  Array.isArray(updaterEndpoints) && updaterEndpoints.length > 0,
  'desktop updater endpoints are missing',
);

// This fork publishes update manifests to its own GitHub Releases. Protect the
// fork against silently switching users to the upstream project's update CDN.
const updateSource = await readFile(
  join(repoRoot, 'openless-all/app/src-tauri/src/update_source.rs'),
  'utf8',
);
assert(
  updateSource.includes(currentRepo),
  'update_source.rs must use the current fork',
);
for (const endpoint of updaterEndpoints) {
  assert(
    endpoint.includes(currentRepo) && endpoint.includes('/releases/latest/download/'),
    `desktop updater endpoint must point at the fork release: ${endpoint}`,
  );
  assert(
    endpoint.includes('{{target}}') && endpoint.includes('{{arch}}'),
    `desktop updater endpoint must keep the {{target}}/{{arch}} placeholders: ${endpoint}`,
  );
}

console.log('repository-owner-contract.test.mjs passed');
