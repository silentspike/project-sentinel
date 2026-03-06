// Control Panel: Steuert das Cortex Gateway via Dashboard Proxy.
// 6 Sektionen: Quick Actions, Provider, LLM Params, Pipeline Hardening, Guardrails, Live Config.
// KEIN innerHTML — nur textContent + DOM API.

let controlState = {
  connected: false,
  paused: false,
  config: null,
  health: null,
};

// API-Key aus sessionStorage (User gibt ihn einmal ein)
function getApiKey() {
  return sessionStorage.getItem('sentinel_api_key') || '';
}

function authHeaders() {
  const key = getApiKey();
  if (!key) return {};
  return { 'Authorization': 'Bearer ' + key };
}

async function controlFetch(url, opts = {}) {
  const headers = { ...authHeaders(), ...(opts.headers || {}) };
  const resp = await fetch(url, { ...opts, headers });
  return resp;
}

// ── Status laden ──────────────────────────────────

async function loadControlStatus() {
  try {
    const resp = await fetch('/api/control/status');
    const data = await resp.json();
    controlState = data;
    return data;
  } catch {
    controlState.connected = false;
    return controlState;
  }
}

// ── Sektion 1: Quick Actions ──────────────────────

function renderQuickActions(container) {
  container.textContent = '';

  const section = el('div', 'control-section');
  section.appendChild(sectionTitle('Quick Actions'));

  const row = el('div', 'control-row');

  // Status-Indikator
  const statusEl = el('div', 'control-status');
  const statusDot = el('span', controlState.paused ? 'status-dot paused' : 'status-dot running');
  const statusText = el('span', 'status-text');
  statusText.textContent = controlState.paused ? 'Pausiert' : 'Aktiv';
  statusEl.appendChild(statusDot);
  statusEl.appendChild(statusText);
  row.appendChild(statusEl);

  // Pause/Resume Button
  const btn = el('button', controlState.paused ? 'control-btn resume' : 'control-btn pause');
  btn.id = controlState.paused ? 'resume-btn' : 'pause-btn';
  btn.textContent = controlState.paused ? 'Resume' : 'Pause';
  btn.addEventListener('click', async () => {
    btn.disabled = true;
    const endpoint = controlState.paused ? '/api/control/resume' : '/api/control/pause';
    try {
      const resp = await controlFetch(endpoint, { method: 'POST' });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({}));
        showFeedback(container, 'Fehler: ' + (err.error || resp.statusText), true);
      } else {
        showFeedback(container, controlState.paused ? 'Resumed' : 'Pausiert');
      }
    } catch (e) {
      showFeedback(container, 'Verbindungsfehler: ' + e.message, true);
    }
    await loadControlStatus();
    renderControl();
  });
  row.appendChild(btn);

  // Connection-Status
  const connEl = el('div', 'control-connection');
  connEl.textContent = controlState.connected ? 'Gateway verbunden' : 'Gateway nicht erreichbar';
  connEl.className = controlState.connected ? 'control-connection connected' : 'control-connection disconnected';
  row.appendChild(connEl);

  section.appendChild(row);
  container.appendChild(section);
}

// ── Sektion 2: Provider Management ────────────────

function renderProviderSection(container) {
  const section = el('div', 'control-section');
  section.appendChild(sectionTitle('Provider Management'));

  const cfg = controlState.config;
  if (!cfg) {
    section.appendChild(noDataMsg());
    container.appendChild(section);
    return;
  }

  // Primary Provider
  const providerRow = el('div', 'control-row');
  const providerLabel = el('label', 'control-label');
  providerLabel.textContent = 'Primary Provider:';
  providerRow.appendChild(providerLabel);

  const providerSelect = document.createElement('select');
  providerSelect.id = 'provider-select';
  providerSelect.className = 'control-select';
  for (const p of ['claude-code', 'ollama', 'claude']) {
    const opt = document.createElement('option');
    opt.value = p;
    opt.textContent = p;
    if (p === cfg.primary_provider) opt.selected = true;
    providerSelect.appendChild(opt);
  }
  providerSelect.addEventListener('change', async () => {
    const resp = await controlFetch('/api/control/provider', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', ...authHeaders() },
      body: JSON.stringify({ provider: providerSelect.value }),
    });
    if (resp.ok) {
      showFeedback(container, 'Provider gewechselt: ' + providerSelect.value);
      await loadControlStatus();
      renderControl();
    } else {
      const err = await resp.json().catch(() => ({}));
      showFeedback(container, 'Fehler: ' + (err.error || resp.statusText), true);
    }
  });
  providerRow.appendChild(providerSelect);
  section.appendChild(providerRow);

  // Agent Overrides
  const overrides = cfg.agent_overrides || {};
  const overrideKeys = Object.keys(overrides);
  if (overrideKeys.length > 0) {
    const overrideTitle = el('div', 'control-subtitle');
    overrideTitle.textContent = 'Agent-Overrides:';
    section.appendChild(overrideTitle);

    for (const agentId of overrideKeys) {
      const row = el('div', 'control-row compact');
      const label = el('span', 'control-label');
      label.textContent = agentId + ': ' + overrides[agentId];
      row.appendChild(label);

      const removeBtn = el('button', 'control-btn-small danger');
      removeBtn.textContent = 'Entfernen';
      removeBtn.addEventListener('click', async () => {
        await controlFetch('/api/control/agent-provider', {
          method: 'DELETE',
          headers: { 'Content-Type': 'application/json', ...authHeaders() },
          body: JSON.stringify({ agent_id: agentId }),
        });
        await loadControlStatus();
        renderControl();
      });
      row.appendChild(removeBtn);
      section.appendChild(row);
    }
  }

  container.appendChild(section);
}

// ── Sektion 3: LLM Parameters ─────────────────────

function renderLlmParams(container) {
  const section = el('div', 'control-section');
  section.appendChild(sectionTitle('LLM Parameter'));

  const cfg = controlState.config;
  if (!cfg) {
    section.appendChild(noDataMsg());
    container.appendChild(section);
    return;
  }

  // Temperature
  const tempRow = el('div', 'control-row');
  tempRow.appendChild(labelEl('Temperature:'));
  const tempInput = document.createElement('input');
  tempInput.type = 'number';
  tempInput.id = 'temperature-input';
  tempInput.className = 'control-input';
  tempInput.min = '0';
  tempInput.max = '2';
  tempInput.step = '0.1';
  tempInput.value = String(cfg.temperature ?? 0.7);
  tempRow.appendChild(tempInput);
  section.appendChild(tempRow);

  // Max Tokens
  const tokRow = el('div', 'control-row');
  tokRow.appendChild(labelEl('Max Tokens:'));
  const tokInput = document.createElement('input');
  tokInput.type = 'number';
  tokInput.id = 'max-tokens-input';
  tokInput.className = 'control-input';
  tokInput.min = '1';
  tokInput.step = '256';
  tokInput.value = String(cfg.max_tokens ?? 4096);
  tokRow.appendChild(tokInput);
  section.appendChild(tokRow);

  // Rate Limit
  const rateRow = el('div', 'control-row');
  rateRow.appendChild(labelEl('Rate Limit (RPS):'));
  const rateInput = document.createElement('input');
  rateInput.type = 'number';
  rateInput.id = 'rate-limit-input';
  rateInput.className = 'control-input';
  rateInput.min = '0';
  rateInput.step = '1';
  rateInput.value = String(cfg.rate_limit_rps ?? 0);
  rateRow.appendChild(rateInput);
  section.appendChild(rateRow);

  // Apply Button
  const applyBtn = el('button', 'control-btn primary');
  applyBtn.id = 'apply-llm-params';
  applyBtn.textContent = 'Anwenden';
  applyBtn.addEventListener('click', async () => {
    const updates = {
      temperature: parseFloat(tempInput.value),
      max_tokens: parseInt(tokInput.value, 10),
      rate_limit_rps: parseFloat(rateInput.value),
    };
    const resp = await controlFetch('/api/control/config', {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json', ...authHeaders() },
      body: JSON.stringify(updates),
    });
    if (resp.ok) {
      showFeedback(container, 'Parameter angewendet');
      await loadControlStatus();
      renderControl();
    } else {
      const err = await resp.json().catch(() => ({}));
      showFeedback(container, 'Fehler: ' + (err.error || resp.statusText), true);
    }
  });
  section.appendChild(applyBtn);

  container.appendChild(section);
}

// ── Sektion 4: Pipeline Hardening (#144) ──────────

function renderPipelineHardening(container) {
  const section = el('div', 'control-section');
  section.appendChild(sectionTitle('Pipeline Hardening'));

  const cfg = controlState.config;
  if (!cfg) {
    section.appendChild(noDataMsg());
    container.appendChild(section);
    return;
  }

  // Personality Guard Toggle
  const guardRow = el('div', 'control-row');
  guardRow.appendChild(labelEl('Personality Guard:'));
  const guardToggle = document.createElement('input');
  guardToggle.type = 'checkbox';
  guardToggle.id = 'personality-guard-toggle';
  guardToggle.className = 'control-toggle';
  guardToggle.checked = cfg.personality_guard_enabled ?? false;
  guardToggle.addEventListener('change', async () => {
    await patchConfig({ personality_guard_enabled: guardToggle.checked });
  });
  guardRow.appendChild(guardToggle);

  // Drift Threshold
  const driftLabel = el('span', 'control-label-inline');
  driftLabel.textContent = ' Drift-Threshold: ' + (cfg.drift_threshold ?? 0.7).toFixed(2);
  guardRow.appendChild(driftLabel);

  const driftInput = document.createElement('input');
  driftInput.type = 'range';
  driftInput.id = 'drift-threshold-slider';
  driftInput.className = 'control-slider';
  driftInput.min = '0';
  driftInput.max = '1';
  driftInput.step = '0.05';
  driftInput.value = String(cfg.drift_threshold ?? 0.7);
  driftInput.addEventListener('input', () => {
    driftLabel.textContent = ' Drift-Threshold: ' + parseFloat(driftInput.value).toFixed(2);
  });
  driftInput.addEventListener('change', async () => {
    await patchConfig({ drift_threshold: parseFloat(driftInput.value) });
  });
  guardRow.appendChild(driftInput);
  section.appendChild(guardRow);

  // Quality Gate Toggle
  const qualRow = el('div', 'control-row');
  qualRow.appendChild(labelEl('Quality Gate:'));
  const qualToggle = document.createElement('input');
  qualToggle.type = 'checkbox';
  qualToggle.id = 'quality-gate-toggle';
  qualToggle.className = 'control-toggle';
  qualToggle.checked = cfg.quality_gate_enabled ?? false;
  qualToggle.addEventListener('change', async () => {
    await patchConfig({ quality_gate_enabled: qualToggle.checked });
  });
  qualRow.appendChild(qualToggle);

  const qualThreshLabel = el('span', 'control-label-inline');
  qualThreshLabel.textContent = ' Threshold: ' + (cfg.quality_threshold ?? 2);
  qualRow.appendChild(qualThreshLabel);

  const qualMax = el('span', 'control-label-inline');
  qualMax.textContent = ' Max Regen: ' + (cfg.quality_max_regen ?? 1);
  qualRow.appendChild(qualMax);
  section.appendChild(qualRow);

  // Narrative Nudge
  const nudgeRow = el('div', 'control-row');
  nudgeRow.appendChild(labelEl('Narrative Nudge:'));
  const nudgeTextarea = document.createElement('textarea');
  nudgeTextarea.id = 'narrative-nudge-textarea';
  nudgeTextarea.className = 'control-textarea';
  nudgeTextarea.rows = 2;
  nudgeTextarea.placeholder = 'z.B. "Sei heute besonders kreativ."';
  nudgeTextarea.value = cfg.narrative_nudge ?? '';
  nudgeRow.appendChild(nudgeTextarea);
  section.appendChild(nudgeRow);

  const nudgeBtn = el('button', 'control-btn primary');
  nudgeBtn.id = 'apply-nudge';
  nudgeBtn.textContent = 'Nudge anwenden';
  nudgeBtn.addEventListener('click', async () => {
    await patchConfig({ narrative_nudge: nudgeTextarea.value });
    showFeedback(container, 'Nudge gesetzt');
  });
  section.appendChild(nudgeBtn);

  container.appendChild(section);
}

// ── Sektion 5: Guardrails Status ──────────────────

function renderGuardrailsStatus(container) {
  const section = el('div', 'control-section');
  section.appendChild(sectionTitle('Guardrails Status'));

  const health = controlState.health;
  if (!health) {
    section.appendChild(noDataMsg());
    container.appendChild(section);
    return;
  }

  const items = [
    ['Guardrails aktiv', health.guardrails_enabled ? 'Ja' : 'Nein'],
    ['Circuit Breakers', JSON.stringify(health.circuit_breakers || 'N/A')],
  ];

  for (const [label, value] of items) {
    const row = el('div', 'control-row compact');
    const lbl = el('span', 'control-label');
    lbl.textContent = label + ':';
    row.appendChild(lbl);
    const val = el('span', 'control-value');
    val.textContent = String(value);
    row.appendChild(val);
    section.appendChild(row);
  }

  // Pipeline Metriken laden
  loadPipelineMetrics(section);

  container.appendChild(section);
}

async function loadPipelineMetrics(section) {
  try {
    const resp = await fetch('/api/metrics/pipeline');
    const data = await resp.json();
    if (!data.available) return;

    const metricsTitle = el('div', 'control-subtitle');
    metricsTitle.textContent = 'Pipeline Metriken:';
    section.appendChild(metricsTitle);

    for (const p of data.providers || []) {
      const row = el('div', 'control-row compact');
      const info = el('span', 'control-value mono');
      info.textContent = p.provider +
        ' | Latenz: ' + (p.latency_avg_s * 1000).toFixed(0) + 'ms' +
        ' | Req: ' + p.requests_ok + 'ok/' + p.requests_error + 'err' +
        ' | Tokens: ' + formatNum(p.tokens_input) + 'in/' + formatNum(p.tokens_output) + 'out';
      row.appendChild(info);
      section.appendChild(row);
    }
  } catch { /* ignore */ }
}

// ── Sektion 6: Live Config ────────────────────────

function renderLiveConfig(container) {
  const section = el('div', 'control-section');
  section.appendChild(sectionTitle('Live Config'));

  const pre = document.createElement('pre');
  pre.id = 'live-config-json';
  pre.className = 'control-json';
  pre.textContent = controlState.config
    ? JSON.stringify(controlState.config, null, 2)
    : 'Nicht verfuegbar';
  section.appendChild(pre);

  container.appendChild(section);
}

// ── API Key Eingabe ───────────────────────────────

function renderApiKeySection(container) {
  const section = el('div', 'control-section compact');

  const row = el('div', 'control-row');
  row.appendChild(labelEl('API-Key:'));
  const keyInput = document.createElement('input');
  keyInput.type = 'password';
  keyInput.id = 'api-key-input';
  keyInput.className = 'control-input';
  keyInput.placeholder = 'SENTINEL_DASHBOARD_API_KEY';
  keyInput.value = getApiKey();
  keyInput.addEventListener('change', () => {
    sessionStorage.setItem('sentinel_api_key', keyInput.value);
    showFeedback(container, 'API-Key gespeichert');
  });
  row.appendChild(keyInput);
  section.appendChild(row);

  container.appendChild(section);
}

// ── Helpers ───────────────────────────────────────

async function patchConfig(updates) {
  const resp = await controlFetch('/api/control/config', {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json', ...authHeaders() },
    body: JSON.stringify(updates),
  });
  if (resp.ok) {
    await loadControlStatus();
    renderControl();
  } else {
    const err = await resp.json().catch(() => ({}));
    const container = document.getElementById('view-control');
    if (container) showFeedback(container, 'Fehler: ' + (err.error || 'Unbekannt'), true);
  }
}

function el(tag, className) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  return e;
}

function sectionTitle(text) {
  const h = document.createElement('h3');
  h.className = 'control-section-title';
  h.textContent = text;
  return h;
}

function labelEl(text) {
  const l = el('label', 'control-label');
  l.textContent = text;
  return l;
}

function noDataMsg() {
  const d = el('div', 'control-no-data');
  d.textContent = 'Gateway nicht verbunden';
  return d;
}

function showFeedback(container, msg, isError = false) {
  // Remove existing feedback
  const existing = container.querySelector('.control-feedback');
  if (existing) existing.remove();

  const fb = el('div', isError ? 'control-feedback error' : 'control-feedback success');
  fb.textContent = msg;
  container.prepend(fb);
  setTimeout(() => fb.remove(), 4000);
}

function formatNum(n) {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K';
  return String(n);
}

// ── Main Render ───────────────────────────────────

function renderControl() {
  const container = document.getElementById('view-control');
  if (!container) return;
  container.textContent = '';

  renderApiKeySection(container);
  renderQuickActions(container);
  renderProviderSection(container);
  renderLlmParams(container);
  renderPipelineHardening(container);
  renderGuardrailsStatus(container);
  renderLiveConfig(container);
}

async function initControl() {
  await loadControlStatus();
  renderControl();
}

// Auto-refresh alle 10s wenn Control-Tab aktiv
setInterval(async () => {
  const container = document.getElementById('view-control');
  if (container && container.classList.contains('active')) {
    await loadControlStatus();
    renderControl();
  }
}, 10000);

export { initControl, renderControl };
