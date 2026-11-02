import { createHash, createPublicKey, verify } from 'node:crypto';
import { createReadStream } from 'node:fs';

function minisignLines(encoded) {
  const text = encoded.trim().startsWith('untrusted comment:')
    ? encoded.trim()
    : Buffer.from(encoded.trim(), 'base64').toString('utf8').trim();
  return text.split(/\r?\n/);
}

// Tauri wraps minisign text in Base64. Verify both the prehashed payload and
// the trusted comment, not just the key ID (which is not proof of ownership).
export function verifyUpdaterDigest(digest, signature, publicKey, expectedVersion) {
  const keyLines = minisignLines(publicKey);
  const lines = minisignLines(signature);
  const key = Buffer.from(keyLines[1] ?? '', 'base64');
  const packet = Buffer.from(lines[1] ?? '', 'base64');
  if (key.length !== 42 || key.subarray(0, 2).toString() !== 'Ed') {
    throw new Error('Invalid updater public key');
  }
  if (
    packet.length !== 74 ||
    packet.subarray(0, 2).toString() !== 'ED' ||
    !packet.subarray(2, 10).equals(key.subarray(2, 10)) ||
    !lines[2]?.startsWith('trusted comment: ')
  ) {
    throw new Error('Invalid updater signature or signing key does not match the application');
  }
  const ed25519Key = createPublicKey({
    key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), key.subarray(10)]),
    format: 'der',
    type: 'spki',
  });
  const comment = lines[2].slice('trusted comment: '.length);
  if (
    !verify(null, digest, ed25519Key, packet.subarray(10)) ||
    !verify(
      null,
      Buffer.concat([packet.subarray(10), Buffer.from(comment)]),
      ed25519Key,
      Buffer.from(lines[3] ?? '', 'base64'),
    )
  ) {
    throw new Error('Updater signature verification failed');
  }
  // Older Tauri CLI signatures have no version field. When present, require
  // the version bound by the signer to agree with the generated manifest.
  const signedVersion = comment.split('\t').find((field) => field.startsWith('version:'));
  if (expectedVersion && signedVersion && signedVersion.slice(8) !== expectedVersion) {
    throw new Error('Signed updater version does not match the application version');
  }
}

export async function verifyUpdaterArtifact(path, signature, publicKey, expectedVersion) {
  const hash = createHash('blake2b512');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  verifyUpdaterDigest(hash.digest(), signature, publicKey, expectedVersion);
}
