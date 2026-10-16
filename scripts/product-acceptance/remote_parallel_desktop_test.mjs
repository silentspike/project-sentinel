import assert from 'node:assert/strict';
import test from 'node:test';
import { allowedRequest } from './remote_parallel_desktop.mjs';

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
