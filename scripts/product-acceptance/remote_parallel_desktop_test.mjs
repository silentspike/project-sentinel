import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { X509Certificate } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import test, { after, before } from 'node:test';
import tls from 'node:tls';
import { allowedRequest, verifyPeer } from './remote_parallel_desktop.mjs';

let root;
const certificates = {};
function openssl(...args) {
  execFileSync('openssl', args, { cwd: root, stdio: 'pipe', timeout: 10000 });
}
function fixture(name, san, expired = false) {
  const key = path.join(root, name + '.key');
  const pem = path.join(root, name + '.pem');
  const csr = path.join(root, name + '.csr');
  openssl('req', '-new', '-newkey', 'rsa:2048', '-nodes', '-keyout', key,
    '-subj', '/CN=' + name, '-addext', 'subjectAltName=' + san, '-out', csr);
  openssl('x509', '-req', '-in', csr, '-signkey', key, '-days', '1',
    '-copy_extensions', 'copy', '-out', pem);
  let certPath = pem;
  if (expired) {
    const database = path.join(root, 'index');
    const serial = path.join(root, 'serial');
    const config = path.join(root, 'ca.cnf');
    fs.writeFileSync(database, '');
    fs.writeFileSync(serial, '01\n');
    fs.writeFileSync(config, '[ca]\ndefault_ca=fixture\n[fixture]\n' +
      `database=${database}\nserial=${serial}\nnew_certs_dir=${root}\n` +
      `certificate=${pem}\nprivate_key=${key}\n` +
      'default_md=sha256\npolicy=subject\ncopy_extensions=copy\n[subject]\ncommonName=supplied\n');
    certPath = path.join(root, 'expired-final.pem');
    openssl('ca', '-selfsign', '-batch', '-config', config, '-in', csr,
      '-startdate', '20000101000000Z', '-enddate', '20000102000000Z', '-out', certPath);
  }
  return { key: fs.readFileSync(key), cert: fs.readFileSync(certPath) };
}
before(() => {
  root = fs.mkdtempSync(path.join(process.cwd(), '.acceptance-tls-'));
  fs.chmodSync(root, 0o700);
  certificates.valid = fixture('valid', 'DNS:localhost,IP:127.0.0.1');
  certificates.foreign = fixture('foreign', 'DNS:localhost,IP:127.0.0.1');
  certificates.wrongName = fixture('wrong-name', 'DNS:other.invalid');
  certificates.expired = fixture('expired', 'DNS:localhost,IP:127.0.0.1', true);
  const caKey = path.join(root, 'anchor.key');
  const caPem = path.join(root, 'anchor.pem');
  openssl('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', caKey,
    '-out', caPem, '-days', '1', '-subj', '/CN=Test Anchor',
    '-addext', 'basicConstraints=critical,CA:TRUE',
    '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1');
  const csr = path.join(root, 'same-key.csr');
  const pem = path.join(root, 'same-key.pem');
  openssl('req', '-new', '-key', caKey, '-subj', '/CN=Different Certificate',
    '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1', '-out', csr);
  openssl('x509', '-req', '-in', csr, '-CA', caPem, '-CAkey', caKey,
    '-set_serial', '2', '-days', '1', '-copy_extensions', 'copy', '-out', pem);
  certificates.anchor = new X509Certificate(fs.readFileSync(caPem));
  certificates.sameKey = { key: fs.readFileSync(caKey), cert: fs.readFileSync(pem) };
});
after(() => { if (root) fs.rmSync(root, { recursive: true, force: true }); });

async function withPeer(fixture, observe) {
  const server = tls.createServer(fixture, socket => socket.end());
  server.on('tlsClientError', () => {});
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  try {
    await observe('https://127.0.0.1:' + server.address().port);
  } finally {
    await new Promise(resolve => server.close(resolve));
  }
}

const origin = 'https://console.example.invalid';
test('read-only requests and exact session routes allowed', () => {
  assert(allowedRequest('GET', origin + '/api/v1/delivery/lineage', origin));
  assert(allowedRequest('HEAD', origin + '/', origin));
  assert(allowedRequest('POST', origin + '/api/auth/login', origin));
  assert(allowedRequest('POST', origin + '/api/wt-ticket', origin));
});
test('business, provider, cross-origin, and route lookalikes rejected', () => {
  for (const route of ['/operator/chat', '/api/gaia', '/v1/messages', '/api/auth/login/other']) {
    assert(!allowedRequest('POST', origin + route, origin));
  }
  assert(!allowedRequest('GET', 'https://external.example.invalid/', origin));
  assert(!allowedRequest('DELETE', origin + '/api/auth/login', origin));
});

test('SSH-read self-signed anchor validates peer identity and exact certificate', async () => {
  await withPeer(certificates.valid, url => verifyPeer(url, new X509Certificate(certificates.valid.cert)));
});
test('foreign self-signed peer is rejected by TLS trust validation', async () => {
  await withPeer(certificates.foreign, url => assert.rejects(
    verifyPeer(url, new X509Certificate(certificates.valid.cert)),
    error => error.code === 'DEPTH_ZERO_SELF_SIGNED_CERT'));
});
test('trusted certificate with a different hostname is rejected', async () => {
  await withPeer(certificates.wrongName, url => assert.rejects(
    verifyPeer(url, new X509Certificate(certificates.wrongName.cert)),
    error => error.code === 'ERR_TLS_CERT_ALTNAME_INVALID'));
});
test('expired trusted certificate is rejected', async () => {
  await withPeer(certificates.expired, url => assert.rejects(
    verifyPeer(url, new X509Certificate(certificates.expired.cert)),
    error => error.code === 'CERT_HAS_EXPIRED'));
});
test('valid trusted same-SPKI substitute still fails the exact certificate pin', async () => {
  const leaf = new X509Certificate(certificates.sameKey.cert);
  assert.deepEqual(leaf.publicKey.export({ type: 'spki', format: 'der' }),
    certificates.anchor.publicKey.export({ type: 'spki', format: 'der' }));
  await withPeer(certificates.sameKey, url => assert.rejects(
    verifyPeer(url, certificates.anchor), /certificate pin mismatch/));
});
