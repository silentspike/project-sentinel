// Trusted main-branch reporter. Never check out or execute candidate code here.
const crypto = require('node:crypto');
const fs = require('node:fs');
const child = require('node:child_process');
const path = require('node:path');

const NAME = 'candidate-qualification-shadow';
const REPOSITORY = 'silentspike/project-sentinel';

function authenticate(encoded, signature, key) {
  if (!key || key.length < 32 || !/^[a-f0-9]{64}$/.test(signature || '') ||
      typeof encoded !== 'string' || encoded.length > 60000 ||
      !/^[A-Za-z0-9+/]+={0,2}$/.test(encoded)) throw new Error('Invalid receipt authentication');
  const raw = Buffer.from(encoded, 'base64');
  if (raw.toString('base64') !== encoded) throw new Error('Invalid receipt encoding');
  const expected = crypto.createHmac('sha256', key).update(raw).digest();
  if (!crypto.timingSafeEqual(expected, Buffer.from(signature, 'hex')))
    throw new Error('Invalid receipt authentication');
  return raw;
}

async function report({ github, context, env = process.env, verify }) {
  if (context.eventName !== 'workflow_dispatch' || context.ref !== 'refs/heads/main' ||
      `${context.repo.owner}/${context.repo.repo}` !== REPOSITORY)
    throw new Error('Reporter must run from protected repository main');
  const raw = authenticate(env.RECEIPT_B64, env.RECEIPT_SIGNATURE, env.RECEIPT_KEY);
  if (!/^[0-9a-f]{64}$/.test(env.PLAN_SHA256 || ''))
    throw new Error('Independently frozen qualification plan missing');
  const receipt = JSON.parse(raw.toString('utf8'));
  if (receipt.repository !== REPOSITORY || !Number.isSafeInteger(receipt.pr) || receipt.pr <= 0 ||
      !/^[0-9a-f]{40}$/.test(receipt.head_sha || '') || !/^[0-9a-f]{40}$/.test(receipt.base_sha || ''))
    throw new Error('Invalid repository candidate identity');
  const params = { owner: context.repo.owner, repo: context.repo.repo };
  const { data: pr } = await github.rest.pulls.get({ ...params, pull_number: receipt.pr });
  if (pr.state !== 'open' || pr.draft || pr.head.repo.full_name !== REPOSITORY ||
      pr.base.ref !== 'main' || pr.head.sha !== receipt.head_sha || pr.base.sha !== receipt.base_sha)
    throw new Error('Fork, stale, draft or closed candidate');
  const { data: commit } = await github.rest.git.getCommit({ ...params, commit_sha: receipt.head_sha });
  if (commit.tree.sha !== receipt.tree_sha) throw new Error('Candidate tree mismatch');
  const passed = verify(raw, receipt, env.PLAN_SHA256);
  const sha = crypto.createHash('sha256').update(raw).digest('hex');
  const request = {
    ...params, name: NAME, head_sha: receipt.head_sha, external_id: `receipt:${sha}`,
    status: 'completed', conclusion: passed ? 'success' : 'failure',
    details_url: `https://github.com/${REPOSITORY}/actions/runs/${context.runId}`,
    output: { title: passed ? 'Shadow candidate verified' : 'Shadow candidate rejected',
      summary: `Receipt SHA256: ${sha}\nSource head: ${receipt.head_sha}\nTree: ${receipt.tree_sha}\n` +
        'Shadow evidence only. Hosted ci-pass remains required. No production acceptance is implied.' },
  };
  // Repeated delivery of the identical receipt updates its original check.
  const runs = await github.paginate(github.rest.checks.listForRef, {
    ...params, ref: receipt.head_sha, check_name: NAME, per_page: 100,
  });
  const existing = runs.filter(r => r.app?.id === 15368 && r.external_id === request.external_id)
    .sort((a, b) => b.id - a.id)[0];
  if (existing) {
    const { head_sha, ...update } = request;
    await github.rest.checks.update({ ...update, check_run_id: existing.id });
  } else await github.rest.checks.create(request);
  if (!passed) throw new Error('Candidate qualification failed');
}

function verifyLocal(raw, receipt, expectedPlanDigest) {
  const scratch = fs.mkdtempSync('/work/tmp/sentinel-ci-report/receipt-');
  try {
    const filename = path.join(scratch, 'receipt.json');
    fs.writeFileSync(filename, raw, { mode: 0o600, flag: 'wx' });
    const policy = JSON.parse(fs.readFileSync('scripts/ci/qualification-policy.json', 'utf8'));
    const args = ['scripts/ci/candidate.py', 'verify', '--receipt', filename,
      '--head', receipt.head_sha, '--base', receipt.base_sha, '--plan-sha256', expectedPlanDigest];
    for (const gate of policy.required_gates) args.push('--required-gate', gate);
    const result = child.spawnSync('python3', args, {
      encoding: 'utf8', timeout: 30000,
      env: { PATH: process.env.PATH, HOME: process.env.HOME, PYTHONDONTWRITEBYTECODE: '1' },
    });
    // No raw receipt, command, infrastructure identity or credential is logged.
    return result.status === 0;
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
}

module.exports = { authenticate, report, verifyLocal, NAME };
