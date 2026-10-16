const {test} = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const {report, authenticate, NAME} = require('../report-candidate.cjs');

function fixture() {
  const receipt = {repository: 'silentspike/project-sentinel', pr: 123,
    head_sha: 'a'.repeat(40), base_sha: 'b'.repeat(40), tree_sha: 'c'.repeat(40)};
  const raw = Buffer.from(JSON.stringify(receipt));
  const key = 'test-only-reporting-key-not-a-credential';
  const env = {RECEIPT_B64: raw.toString('base64'), RECEIPT_KEY: key,
    PLAN_SHA256: 'f'.repeat(64),
    RECEIPT_SIGNATURE: crypto.createHmac('sha256', key).update(raw).digest('hex')};
  const context = {repo: {owner: 'silentspike', repo: 'project-sentinel'},
    eventName: 'workflow_dispatch', ref: 'refs/heads/main', runId: 123};
  const pr = {state: 'open', draft: false, head: {sha: receipt.head_sha, repo: {full_name: receipt.repository}},
    base: {sha: receipt.base_sha, ref: 'main'}};
  const calls = [];
  const existing = [];
  const github = {rest: {pulls: {get: async () => ({data: pr})},
    git: {getCommit: async () => ({data: {tree: {sha: receipt.tree_sha}}})},
    checks: {listForRef: async () => {}, create: async (r) => calls.push(r), update: async (r) => calls.push(r)}},
    paginate: async () => existing};
  return {receipt, raw, env, context, pr, github, calls, existing, verify: () => true};
}

test('authenticates exact bytes and never accepts missing or invalid signatures', () => {
  const f = fixture();
  assert.deepEqual(authenticate(f.env.RECEIPT_B64, f.env.RECEIPT_SIGNATURE, f.env.RECEIPT_KEY), f.raw);
  for (const mutate of [e => e.RECEIPT_SIGNATURE = '0'.repeat(64), e => e.RECEIPT_KEY = '',
    e => e.RECEIPT_B64 += '!', e => e.RECEIPT_SIGNATURE = '', e => e.RECEIPT_B64 = 'YQ===']) {
    const e = {...f.env}; mutate(e);
    assert.throws(() => authenticate(e.RECEIPT_B64, e.RECEIPT_SIGNATURE, e.RECEIPT_KEY));
  }
});

test('creates only a shadow Actions check for the exact PR head', async () => {
  const f = fixture();
  await report(f);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].name, NAME);
  assert.equal(f.calls[0].head_sha, f.receipt.head_sha);
  assert.equal(f.calls[0].conclusion, 'success');
  assert.ok(!f.calls[0].output.summary.includes(f.env.RECEIPT_KEY));
});

test('never executes verification or creates checks for forks or stale inputs', async () => {
  for (const mutate of [f => f.pr.head.repo.full_name = 'outside/fork',
    f => f.pr.head.sha = 'd'.repeat(40), f => f.pr.base.sha = 'd'.repeat(40),
    f => f.pr.draft = true, f => f.pr.state = 'closed',
    f => f.context.ref = 'refs/heads/feature', f => f.context.eventName = 'pull_request',
    f => f.context.repo.owner = 'outside',
    f => f.github.rest.git.getCommit = async () => ({data: {tree: {sha: 'd'.repeat(40)}}})]) {
    const f = fixture(); mutate(f);
    let executed = false;
    f.verify = () => {executed = true; return true;};
    await assert.rejects(report(f));
    assert.equal(executed, false);
    assert.equal(f.calls.length, 0);
  }
});

test('authenticated failed qualification reports failure and fails workflow', async () => {
  const f = fixture();
  f.verify = () => false;
  await assert.rejects(report(f), /qualification failed/);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].conclusion, 'failure');
});

test('trusted workflow variable binds the independently frozen plan', async () => {
  for (const value of ['', 'not-a-digest', undefined]) {
    const f = fixture(); f.env.PLAN_SHA256 = value;
    await assert.rejects(report(f), /frozen qualification plan missing/);
    assert.equal(f.calls.length, 0);
  }
  const f = fixture();
  let binding;
  f.verify = (_raw, _receipt, expected) => {binding = expected; return true;};
  await report(f);
  assert.equal(binding, f.env.PLAN_SHA256);
});

test('duplicate receipt delivery updates original Actions check, not foreign app', async () => {
  const f = fixture();
  const external_id = `receipt:${crypto.createHash('sha256').update(f.raw).digest('hex')}`;
  f.existing.push({id: 44, external_id, app: {id: 15368}}, {id: 45, external_id, app: {id: 999}});
  await report(f);
  assert.equal(f.calls[0].check_run_id, 44);
});

function v2Fixture() {
  const f = fixture();
  f.receipt.schema_version = 2;
  f.receipt.qualification_plan = {schema_version: 2, applicability_digest: '1'.repeat(64),
    ignored_applicability: {required_count: 42, not_applicable_count: 2,
      not_applicable_status: 'NOTEXECUTED_NOTPASS'}};
  f.raw = Buffer.from(JSON.stringify(f.receipt));
  f.env.RECEIPT_B64 = f.raw.toString('base64');
  f.env.RECEIPT_SIGNATURE = crypto.createHmac('sha256', f.env.RECEIPT_KEY).update(f.raw).digest('hex');
  return f;
}

test('v2 authenticates unchanged exact bytes and preserves the independent plan binding', async () => {
  const f = v2Fixture();
  f.verify = (raw, receipt, expectedPlan) => {
    assert.deepEqual(raw, f.raw);
    assert.equal(receipt.schema_version, 2);
    assert.equal(expectedPlan, f.env.PLAN_SHA256);
    return true;
  };
  await report(f);
  assert.equal(f.calls[0].external_id, `receipt:${crypto.createHash('sha256').update(f.raw).digest('hex')}`);
  assert.equal(f.calls[0].name, NAME);
});

test('v2 map digest and N/A accounting tampering cannot reach verification with an old HMAC', async () => {
  for (const mutate of [p => p.applicability_digest = '2'.repeat(64),
    p => p.ignored_applicability.required_count--,
    p => p.ignored_applicability.not_applicable_count++,
    p => p.ignored_applicability.not_applicable_status = 'PASS']) {
    const f = v2Fixture();
    mutate(f.receipt.qualification_plan);
    f.env.RECEIPT_B64 = Buffer.from(JSON.stringify(f.receipt)).toString('base64');
    let executed = false;
    f.verify = () => {executed = true; return true;};
    await assert.rejects(report(f), /authentication/);
    assert.equal(executed, false);
    assert.equal(f.calls.length, 0);
  }
});

test('v2 authenticated but unqualified coverage is a shadow failure, never ci-pass', async () => {
  const f = v2Fixture();
  f.verify = () => false;
  await assert.rejects(report(f), /qualification failed/);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].conclusion, 'failure');
  assert.notEqual(f.calls[0].name, 'ci-pass');
});
