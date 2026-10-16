#!/usr/bin/env node
// Read-only business journey. Login/ticket creation is the only allowed POST.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, X509Certificate } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import tls from 'node:tls';
import { pathToFileURL } from 'node:url';

export function allowedRequest(method, requestUrl, origin) {
  const url = new URL(requestUrl);
  if (url.origin !== origin) return false;
  if (method === 'GET' || method === 'HEAD') return true;
  return method === 'POST' && ['/api/auth/login', '/api/wt-ticket'].includes(url.pathname);
}

function protectedRead(config, credentials) {
  const source = `
import hashlib,json,shlex,subprocess
from pathlib import Path
c=json.loads(${JSON.stringify(JSON.stringify(config))})
p=Path('/opt/sentinel/release-manifest.json')
assert hashlib.sha256(p.read_bytes()).hexdigest()==c['manifest_sha256']
result={'manifest_sha256':c['manifest_sha256']}
result['runtime']=subprocess.check_output(['systemctl','show','sentinel-daemon','sentinel-dashboard-backend','-p','MainPID','-p','NRestarts'],text=True,timeout=5)
if ${credentials ? 'True' : 'False'}:
    result['cert_pem']=Path(c['certificate_file']).read_text()
    lines=Path(c['credential_env_file']).read_text().splitlines()
    keys=[shlex.split(line.split('=',1)[1]) for line in lines if line.startswith('SENTINEL_DASHBOARD_API_KEY=')]
    assert len(keys)==1 and len(keys[0])==1 and keys[0][0]
    result['key']=keys[0][0]
print(json.dumps(result))
`;
  return JSON.parse(execFileSync('ssh', ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
    config.ssh_target, 'sudo -n python3 -'], { input: source, encoding: 'utf8', timeout: 25000,
    maxBuffer: 65536, stdio: ['pipe', 'pipe', 'pipe'], cwd: process.cwd() }));
}

export async function verifyPeer(origin, certificate) {
  const url = new URL(origin);
  const expected = createHash('sha256').update(certificate.raw).digest('hex');
  await new Promise((resolve, reject) => {
    const socket = tls.connect({ host: url.hostname, port: Number(url.port || 443),
      ca: certificate.toString(), rejectUnauthorized: true }, () => {
      // The SSH-read anchor retains chain, validity and hostname validation.
      const raw = socket.getPeerCertificate().raw;
      if (!raw || createHash('sha256').update(raw).digest('hex') !== expected) {
        socket.destroy(); reject(new Error('certificate pin mismatch')); return;
      }
      socket.end(); resolve();
    });
    socket.setTimeout(10000, () => socket.destroy(new Error('TLS observation timeout')));
    socket.on('error', reject);
  });
}

export async function run(config, output, playwrightModule) {
  assert(['LAB', 'SINGLE_NODE'].includes(config.runtime_target_class));
  const origin = new URL(config.origin).origin;
  assert(new URL(origin).protocol === 'https:');
  assert(/^[A-Za-z0-9_.@-]+$/.test(config.ssh_target));
  assert(/^[0-9a-f]{64}$/.test(config.manifest_sha256));
  assert(/^[A-Za-z0-9._-]{1,128}$/.test(config.tenant_id));
  assert(/^[A-Za-z0-9._-]{1,128}$/.test(config.project_id));
  const out = path.resolve(output);
  assert(out.startsWith('/work/tmp/project-sentinel/'));
  process.umask(0o077);
  delete process.env.DEBUG;
  delete process.env.PWDEBUG;
  fs.mkdirSync(out, { mode: 0o700 });
  const require = createRequire(import.meta.url);
  const { chromium } = require(playwrightModule);
  const before = protectedRead(config, true);
  const cert = new X509Certificate(before.cert_pem);
  await verifyPeer(origin, cert);
  const spki = createHash('sha256').update(cert.publicKey.export({ type: 'spki', format: 'der' })).digest('base64');
  const browser = await chromium.launch({ headless: true, timeout: 30000,
    args: ['--ignore-certificate-errors-spki-list=' + spki], env: { ...process.env, TMPDIR: out } });
  const result = { schema: 1, kind: 'desktop_read_only_observation', status: 'INCOMPLETE',
    runtime_target_class: config.runtime_target_class, viewport: { width: 1440, height: 1000 },
    business_mutations: 0, provider_dispatches_by_harness: 0, auth_session_post_requests: 0,
    blocked_requests: 0, page_errors: 0, product_acceptance: 'NOT_VERIFIED' };
  result.transport_scope = 'HTTPS delivery read only; live WebTransport topics NOT_VERIFIED';
  result.browser_version = browser.version();
  try {
    const context = await browser.newContext({ viewport: result.viewport, serviceWorkers: 'block' });
    await context.route('**/*', async route => {
      const request = route.request();
      if (!allowedRequest(request.method(), request.url(), origin)) {
        result.blocked_requests++; await route.abort(); return;
      }
      if (request.method() === 'POST') result.auth_session_post_requests++;
      await route.continue();
    });
    const page = await context.newPage();
    page.setDefaultTimeout(20000);
    page.on('pageerror', () => result.page_errors++);
    const scope = new URLSearchParams({ tenant_id: config.tenant_id, project_id: config.project_id });
    await page.goto(origin + '/?' + scope, { waitUntil: 'domcontentloaded', timeout: 30000 });
    const hashResponse = await page.evaluate(async () => (await fetch('/api/cert-hash')).json());
    assert.equal(hashResponse.algorithm, 'sha-256');
    assert.equal(hashResponse.hash, createHash('sha256').update(cert.raw).digest('base64'));
    await page.getByTestId('login-key').fill(before.key);
    before.key = '';
    await page.getByTestId('login-submit').click();
    await page.getByTestId('open-delivery').waitFor({ state: 'visible' });
    await page.screenshot({ path: path.join(out, 'workspace.png') });
    const lineageResponse = page.waitForResponse(r => new URL(r.url()).pathname === '/api/v1/delivery/lineage');
    await page.getByTestId('open-delivery').click();
    await page.getByTestId('view-delivery').waitFor({ state: 'visible' });
    result.lineage_http_status = (await lineageResponse).status();
    await page.getByTestId('delivery-loading').waitFor({ state: 'hidden' });
    result.lineage_nodes = await page.getByTestId('delivery-lineage-node').count();
    result.lineage_digests = await page.getByTestId('delivery-digest').count();
    result.lineage_authorities = await page.getByTestId('delivery-authority').count();
    result.lineage_invalid = await page.getByTestId('delivery-invalid').count() > 0;
    result.lineage_state = result.lineage_http_status === 404 ? 'INACCESSIBLE_NOT_DELIVERED'
      : result.lineage_http_status === 200 && result.lineage_nodes > 0 && !result.lineage_invalid
        && result.lineage_nodes === result.lineage_digests && result.lineage_nodes === result.lineage_authorities
        ? 'VISIBLE_NOT_PRODUCT_ACCEPTANCE' : 'UNAVAILABLE';
    result.horizontal_overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
    await page.screenshot({ path: path.join(out, 'delivery.png') });
    const after = protectedRead(config, false);
    result.release_stable = before.manifest_sha256 === after.manifest_sha256 && before.runtime === after.runtime;
    result.status = result.release_stable && !result.page_errors && !result.blocked_requests
      && !result.horizontal_overflow && result.lineage_state !== 'UNAVAILABLE' ? 'OBSERVED' : 'INCOMPLETE';
    await context.close();
  } catch {
    // Playwright errors can embed URLs or field values; never export them.
    result.status = 'INCOMPLETE';
    result.reason = 'desktop prerequisite, pin, auth, route, or assertion failure';
  } finally {
    await browser.close();
    const files = fs.readdirSync(out).filter(name => name.endsWith('.png'));
    result.screenshots = files.map(name => ({ file: name,
      sha256: createHash('sha256').update(fs.readFileSync(path.join(out, name))).digest('hex') }));
    fs.writeFileSync(path.join(out, 'result.json'), JSON.stringify(result, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  }
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    const [, , input, output, modulePath] = process.argv;
    assert(input && output && modulePath, 'private config, output, and pinned Playwright module required');
    const result = await run(JSON.parse(fs.readFileSync(input, 'utf8')), output, modulePath);
    console.log(JSON.stringify(result));
    process.exitCode = result.status === 'OBSERVED' ? 0 : 2;
  } catch {
    console.log(JSON.stringify({ status: 'BLOCKED', reason: 'desktop prerequisite or protected input failure' }));
    process.exitCode = 2;
  }
}
