import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import yaml from 'js-yaml';

const workflow = (name) =>
  yaml.load(readFileSync(new URL(`../../../.github/workflows/${name}`, import.meta.url), 'utf8'));
const release = workflow('release-tauri.yml');
const steps = release.jobs.build.steps;
const preflight = steps.find((step) => step.name === 'Check updater signing availability');
assert.equal(preflight.run, 'node scripts/verify-updater-signing.mjs');
assert(preflight.if.includes("env.OPENLESS_RELEASE_IS_TAG == 'true'"));
assert(steps.indexOf(preflight) < steps.findIndex((step) => step.name === 'Build (macOS)'));
const manifest = steps.find((step) => step.name === 'Write updater manifest');
assert(
  manifest.if.includes("env.OPENLESS_RELEASE_IS_TAG == 'true'"),
  'branch builds must not generate release manifests',
);
const publish = steps.find((step) => step.name === 'Create / update release');
assert.equal(publish.with.draft, true, 'publishing to clients remains an administrator decision');
assert(publish.with.files.includes('/latest-*.json'));
assert(publish.with.files.includes('/*.app.tar.gz.sig'));
const testBuild = workflow('test-macos-audio-fix.yml').jobs['macos-arm64'].steps;
assert(testBuild.some((step) => step.run === 'node scripts/verify-updater-signing.mjs'));
assert(
  testBuild.find((step) => step.run === 'bash scripts/build-mac.sh').env.TAURI_SIGNING_PRIVATE_KEY,
);
assert(
  !testBuild.some((step) => step.uses?.includes('action-gh-release')),
  'test builds must not be published',
);
console.log('updater-workflow-contract.test.mjs passed');
