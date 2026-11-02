import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = fileURLToPath(new URL('..', import.meta.url));
const directory = mkdtempSync(join(tmpdir(), 'openless-updater-'));
const app = join(directory, 'app');
const bundle = join(directory, 'bundle');
const version = '2.0.2-Beta.4+build.fixture';
const releaseTag = 'v2.0.2-Beta.4-tauri';
const assetName = `OpenLess_${version}_arm64-v8a.apk`;
const cli = join(appRoot, 'node_modules/@tauri-apps/cli/tauri.js');
const password = 'ephemeral-test-password';

try {
  mkdirSync(join(app, 'scripts'), { recursive: true });
  mkdirSync(join(app, 'src-tauri'));
  mkdirSync(bundle);
  symlinkSync(join(appRoot, 'node_modules'), join(app, 'node_modules'), 'junction');
  for (const script of [
    'write-updater-manifest.mjs',
    'updater-signature.mjs',
    'verify-updater-signing.mjs',
  ]) {
    copyFileSync(join(appRoot, 'scripts', script), join(app, 'scripts', script));
  }
  writeFileSync(join(app, 'package.json'), JSON.stringify({ type: 'module', version }));
  execFileSync(
    process.execPath,
    [cli, 'signer', 'generate', '--ci', '-p', password, '-w', join(directory, 'key')],
    { stdio: 'pipe' },
  );
  const publicKey = readFileSync(join(directory, 'key.pub'), 'utf8').trim();
  const config = { version, plugins: { updater: { pubkey: publicKey } } };
  const writeConfig = () =>
    writeFileSync(join(app, 'src-tauri/tauri.conf.json'), JSON.stringify(config));
  writeConfig();
  const env = {
    ...process.env,
    TAURI_SIGNING_PRIVATE_KEY: readFileSync(join(directory, 'key'), 'utf8').trim(),
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: password,
    OPENLESS_UPDATE_APK_DIR: bundle,
    OPENLESS_UPDATE_ARCH: 'aarch64',
    OPENLESS_UPDATE_REPO: 'dandibbert/openless',
    OPENLESS_UPDATE_MIRROR_BASE_URL: 'https://fastgit.cc/https://github.com',
    OPENLESS_UPDATE_TARGET: 'android',
    OPENLESS_RELEASE_CHANNEL: 'beta',
    OPENLESS_RELEASE_TAG: releaseTag,
  };
  delete env.TAURI_SIGNING_PRIVATE_KEY_PATH;
  const run = (script, overrides = {}, expectedError) => {
    const result = spawnSync(process.execPath, [join(app, 'scripts', script)], {
      env: { ...env, ...overrides },
      encoding: 'utf8',
      timeout: 45_000,
    });
    if (expectedError) {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, expectedError);
      assert(
        !result.stderr.includes(env.TAURI_SIGNING_PRIVATE_KEY),
        'private key must not be logged',
      );
    } else {
      assert.equal(result.status, 0, result.stderr);
    }
  };
  run('verify-updater-signing.mjs');
  run(
    'verify-updater-signing.mjs',
    { TAURI_SIGNING_PRIVATE_KEY: '' },
    /requires TAURI_SIGNING_PRIVATE_KEY/,
  );
  run('verify-updater-signing.mjs', { TAURI_SIGNING_PRIVATE_KEY_PASSWORD: 'wrong' }, /Cannot sign/);
  const originalConfig = config.plugins.updater.pubkey;
  const keyLines = Buffer.from(publicKey, 'base64').toString().trim().split('\n');
  const keyBytes = Buffer.from(keyLines[1], 'base64');
  // Keep the key ID unchanged: the preflight must verify the signature itself.
  keyBytes[41] ^= 1;
  keyLines[1] = keyBytes.toString('base64');
  config.plugins.updater.pubkey = Buffer.from(`${keyLines.join('\n')}\n`).toString('base64');
  writeConfig();
  run('verify-updater-signing.mjs', {}, /verification failed/);
  config.plugins.updater.pubkey = originalConfig;
  writeConfig();

  writeFileSync(join(bundle, assetName), 'fixture');
  execFileSync(
    process.execPath,
    [cli, 'signer', 'sign', join(bundle, assetName), '--app-version', version],
    { env, stdio: 'pipe' },
  );
  const signature = readFileSync(join(bundle, `${assetName}.sig`), 'utf8');
  run('write-updater-manifest.mjs');
  const expected = `https://github.com/dandibbert/openless/releases/download/${releaseTag}/${assetName}`;
  for (const suffix of ['', '-mirror']) {
    const manifest = JSON.parse(
      readFileSync(join(bundle, `latest-android-aarch64-beta${suffix}.json`), 'utf8'),
    );
    assert.equal(
      manifest.url,
      suffix
        ? expected.replace('https://github.com/', 'https://fastgit.cc/https://github.com/')
        : expected,
    );
    assert.equal(manifest.version, version);
    assert.equal(manifest.signature, signature.trim());
  }
  run(
    'write-updater-manifest.mjs',
    { OPENLESS_RELEASE_CHANNEL: 'stable' },
    /prerelease application/,
  );
  run(
    'write-updater-manifest.mjs',
    { OPENLESS_RELEASE_TAG: '' },
    /OPENLESS_RELEASE_TAG is required/,
  );
  run(
    'write-updater-manifest.mjs',
    { OPENLESS_RELEASE_TAG: 'v2.0.2-Beta.99-tauri' },
    /Release tag must match/,
  );
  writeFileSync(join(bundle, assetName), 'tampered fixture');
  run('write-updater-manifest.mjs', {}, /verification failed/);
  writeFileSync(join(bundle, assetName), 'fixture');
  writeFileSync(join(bundle, `${assetName}.sig`), 'not a signature');
  run('write-updater-manifest.mjs', {}, /Invalid updater signature/);
  writeFileSync(join(bundle, `${assetName}.sig`), signature);
  const signatureLines = Buffer.from(signature.trim(), 'base64').toString().trim().split('\n');
  signatureLines[2] += '\ttampered';
  writeFileSync(
    join(bundle, `${assetName}.sig`),
    Buffer.from(signatureLines.join('\n')).toString('base64'),
  );
  run('write-updater-manifest.mjs', {}, /verification failed/);
  execFileSync(
    process.execPath,
    [cli, 'signer', 'sign', join(bundle, assetName), '--app-version', '2.0.2-Beta.99'],
    { env, stdio: 'pipe' },
  );
  run('write-updater-manifest.mjs', {}, /Signed updater version does not match/);

  config.version = '2.0.2+build.fixture-hash';
  writeConfig();
  run('write-updater-manifest.mjs', {}, /versions must match/);
  writeFileSync(
    join(app, 'package.json'),
    JSON.stringify({ type: 'module', version: config.version }),
  );
  execFileSync(
    process.execPath,
    [cli, 'signer', 'sign', join(bundle, assetName), '--app-version', config.version],
    { env, stdio: 'pipe' },
  );
  const stableTag = 'v2.0.2-tauri';
  run('write-updater-manifest.mjs', {
    OPENLESS_RELEASE_CHANNEL: 'stable',
    OPENLESS_RELEASE_TAG: stableTag,
  });
  const stableManifest = JSON.parse(
    readFileSync(join(bundle, 'latest-android-aarch64.json'), 'utf8'),
  );
  assert.equal(
    stableManifest.url,
    `https://github.com/dandibbert/openless/releases/download/${stableTag}/${assetName}`,
  );
  // Older CLI signatures without a bound version remain compatible with the desktop verifier.
  execFileSync(process.execPath, [cli, 'signer', 'sign', join(bundle, assetName)], {
    env,
    stdio: 'pipe',
  });
  run('write-updater-manifest.mjs', {
    OPENLESS_RELEASE_CHANNEL: 'stable',
    OPENLESS_RELEASE_TAG: stableTag,
  });
  const desktopBundle = join(app, 'src-tauri/target/release/bundle');
  for (const [target, arch, subdir, name] of [
    ['darwin', 'aarch64', 'macos', 'OpenLess_aarch64.app.tar.gz'],
    ['darwin', 'x86_64', 'macos', 'OpenLess_x86_64.app.tar.gz'],
    ['windows', 'x86_64', 'nsis', 'OpenLess_2.0.2_x64-setup.exe'],
  ]) {
    mkdirSync(join(desktopBundle, subdir), { recursive: true });
    const artifact = join(desktopBundle, subdir, name);
    writeFileSync(artifact, 'desktop updater fixture');
    const overrides = {
      OPENLESS_UPDATE_TARGET: target,
      OPENLESS_UPDATE_ARCH: arch,
      OPENLESS_RELEASE_CHANNEL: 'stable',
      OPENLESS_RELEASE_TAG: stableTag,
    };
    run('write-updater-manifest.mjs', overrides, /Missing updater signature/);
    execFileSync(
      process.execPath,
      [cli, 'signer', 'sign', artifact, '--app-version', config.version],
      { env, stdio: 'pipe' },
    );
    run('write-updater-manifest.mjs', overrides);
    const manifest = JSON.parse(
      readFileSync(join(desktopBundle, `latest-${target}-${arch}.json`), 'utf8'),
    );
    assert.equal(
      manifest.url,
      `https://github.com/dandibbert/openless/releases/download/${stableTag}/${name}`,
    );
    assert.equal(manifest.version, config.version);
  }
} finally {
  rmSync(directory, { recursive: true, force: true });
}

console.log('write-updater-manifest-contract.test.mjs passed');
