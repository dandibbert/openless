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
const publish = release.jobs.publish.steps.find((step) => step.name === 'Create / update release');
assert.equal(publish.with.draft, true, 'publishing to clients remains an administrator decision');
assert.equal(publish.with.files.trim(), 'release-files/*');
const publishStage = release.jobs.publish.steps.find(
  (step) => step.name === 'Stage release files',
);
assert.match(publishStage.run, /latest-\*\.json/);
assert.match(publishStage.run, /\*\.app\.tar\.gz\.sig/);
const bootstrap = workflow('test-macos-audio-fix.yml');
assert.deepEqual(bootstrap.on.workflow_dispatch.inputs.macos_runner.options, [
  'macos-latest',
  'macos-15',
]);
assert.equal(
  bootstrap.jobs['macos-arm64']['runs-on'],
  "${{ inputs.macos_runner || 'macos-latest' }}",
);
const testBuild = bootstrap.jobs['macos-arm64'].steps;
assert(testBuild.some((step) => step.run === 'node scripts/verify-updater-signing.mjs'));
assert(
  testBuild.find((step) => step.run === 'bash scripts/build-mac.sh').env.TAURI_SIGNING_PRIVATE_KEY,
);
const stage = testBuild.find((step) => step.name === 'Verify and stage updater manifests');
assert.equal(stage.env.OPENLESS_UPDATE_TARGET, 'darwin');
assert.equal(stage.env.OPENLESS_UPDATE_ARCH, 'aarch64');
assert.match(stage.run, /node scripts\/write-updater-manifest\.mjs/);
assert.match(stage.run, /sourceSha: process\.env\.GITHUB_SHA/);
assert.match(stage.run, /OPENLESS_RELEASE_TAG="v\$\{version%%\+\*\}-tauri"/);
const upload = testBuild.find((step) => step.name === 'Upload signed updater bundle');
assert.match(upload.with.path, /latest-darwin-aarch64\*\.json/);
assert.match(upload.with.path, /updater-build-provenance\.json/);
assert(testBuild.indexOf(stage) < testBuild.indexOf(upload));
assert(
  testBuild.indexOf(testBuild.find((step) => step.name === 'Configure macOS')) <
    testBuild.findIndex((step) => step.uses?.startsWith('swatinem/rust-cache@')),
);
assert(
  !testBuild.some((step) => step.uses?.includes('action-gh-release')),
  'test builds must not be published',
);
console.log('updater-workflow-contract.test.mjs passed');
