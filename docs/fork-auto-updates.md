# Automatic updates for dandibbert/openless

The desktop app already checks for updates at startup and periodically when
the automatic-check preference is enabled. An available update opens a
confirmation dialog; it does not silently restart the app during dictation.
The user confirms download/installation and then restarts. Settings also has a
manual update check. Prerelease builds default to the Beta channel unless the
user explicitly selects another channel.

## Two different signatures

`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` sign updater
packages with a Minisign/Ed25519 key. The public half is embedded in both
`openless-all/app/src-tauri/tauri.conf.json` and
`openless-all/app/src-tauri/src/android/updater_logic.rs`. Both must match.
The private key is an encrypted, Base64-encoded Tauri key, not a P12.

`APPLE_CERTIFICATE` is the Base64 content of a macOS code-signing P12;
`APPLE_CERTIFICATE_PASSWORD` unlocks it. The desktop release workflow already
imports these repository secrets. For Developer ID signing and notarization it
also uses `APPLE_ID`, `APPLE_PASSWORD` (an app-specific password), and
`APPLE_TEAM_ID`. A locally generated self-signed certificate is not an Apple
Developer ID certificate and does not remove Gatekeeper warnings. The current
test-DMG workflow uses ad-hoc macOS signing, even when updater signing is enabled.

The updater signature is still required when the macOS app is ad-hoc signed.
Never disable signature verification to make an update install.

## Initial fork key and manual bootstrap

The fork's initial updater key was provisioned independently of the inherited
upstream public key. The matching private key and password are GitHub Actions
repository secrets, never source files or build artifacts. A restricted local
backup is kept outside the checkout at
`~/.config/openless/updater/dandibbert-openless/` on the provisioning host.
Back up the encrypted key and password to an administrator-controlled secure
store; GitHub does not let administrators download secret values again.

Existing installations that embed the inherited key cannot verify this fork's
new signatures. Install the fork-key bootstrap DMG manually once. The first
bootstrap version is `2.0.2-Beta.4+build.20261102`. Future published versions
signed with the same fork key can then install through the app. Do not regenerate
the key on each build or silently replace it after clients have installed it:
losing or rotating it requires a deliberately planned migration or another
manual installation.

Updating the public key in the desktop configuration without updating Android
fails `node scripts/check-android-updater-pubkey.mjs`. Before a signed build,
`node scripts/verify-updater-signing.mjs` signs a temporary challenge and verifies
it with the embedded public key. Incorrect passwords, missing credentials, and
keys with the wrong public half fail without logging private material.

## Test builds versus published updates

An ordinary push to `beta` does not publish an update. The manual
`Test macOS WebKit audio fix DMG` workflow produces an ARM64 DMG and a signed
updater archive as Actions artifacts. It also verifies that archive's actual
signature, prepares tag-pinned direct/mirror manifests, and includes the source
commit and application version in `updater-build-provenance.json`. These files
are ready for an administrator to inspect, but the test workflow does not create
a tag or put a release or manifest in the public feed.

Only an administrator may cut a public release, following `RELEASING.md` and
the relevant platform acceptance gates. Release tags omit build metadata:
for example `v2.0.2-Beta.4-tauri`. All application version locations must agree,
and the tag must match that version. Each later Beta increments the numeric
`Beta.N` component; changing only a commit hash or date in `+build` does not make
the updater consider it newer. Do not move an already-published version tag.

The desktop release workflow uploads installers, signed updater archives, and
verified manifests to a draft. The administrator checks the packages and
publishes the draft as a prerelease for Beta, or as the latest release for
Stable. Publishing makes the feed discoverable; no upstream CDN upload is
needed. Do not publish a draft while matrix jobs are still uploading assets.
The full tag pipeline also builds Android, Linux, and HarmonyOS and needs their
own acceptance/signing configuration; updater secrets do not provide an Android
keystore or Apple notarization credentials.

For macOS, verify that the release includes the archive, its `.sig`, and the
matching direct and mirror manifests for every architecture being shipped.
ARM64 names are `OpenLess_aarch64.app.tar.gz`,
`OpenLess_aarch64.app.tar.gz.sig`, `latest-darwin-aarch64-beta.json`, and
`latest-darwin-aarch64-beta-mirror.json` for Beta. Intel uses `x86_64`. Stable
manifest filenames omit `-beta`.

Stable manifest discovery uses this fork's `releases/latest/download/` endpoint.
Beta discovery reads this fork's published release feed and selects a Beta tag,
then fetches its `latest-*-beta.json` assets. Both channels pin the actual
package download to the manifest's release tag and verify the signature before
installation. Drafts and Actions artifacts are intentionally invisible to the
application's automatic update checks.

## Restoring credentials

On a trusted host with a secure key backup and authenticated administrator `gh`,
restore repository secrets through standard input, never by pasting them into
chat or command arguments:

```sh
gh secret set TAURI_SIGNING_PRIVATE_KEY --repo dandibbert/openless < /secure/path/updater.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --repo dandibbert/openless < /secure/path/password
```

To reuse an existing Apple P12, upload its Base64 content as `APPLE_CERTIFICATE`
and its password as `APPLE_CERTIFICATE_PASSWORD` through GitHub's secret UI or
`gh secret set` standard input. Do not upload the P12 as an updater private key.
