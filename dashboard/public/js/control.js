// Control Panel: Steuert das Cortex Gateway via Dashboard Proxy.
// Sektionen: Quick Actions, Provider, LLM Params, Pipeline Hardening, Guardrails,
// Traffic Control, Live Config, Snapshots.
// KEIN innerHTML — nur textContent + DOM API.

// API-Key: geteiltes In-Memory-Modul (siehe api-key.js)
import { getApiKey, setApiKey, authHeaders } from './api-key.js';

let controlState = {
  connected: false,
  paused: false,
  config: null,
  health: null,
  trafficStats: null,
  platformAnalyses: [],
  platformState: null,
};

async function controlFetch(url, opts = {}) {
  const headers = { ...authHeaders(), ...(opts.headers || {}) };
  const resp = await fetch(url, { ...opts, headers });
  return resp;
}

// ── Status laden ──────────────────────────────────

async function loadControlStatus() {
  async function fetchJsonOrNull(url) {
    try {
      const resp = await fetch(url);
      if (!resp.ok) return null;
      return await resp.json();
    } catch {
      return null;
    }
  }

  const [statusData, trafficStats, platformAnalyses, platformState] = await Promise.all([
    fetchJsonOrNull('/api/control/status'),
    fetchJsonOrNull('/api/control/traffic-stats'),
    fetchJsonOrNull('/api/control/platform-analyses'),
    fetchJsonOrNull('/api/control/platform-state'),
  ]);

  if (!statusData) {
    controlState = {
      ...controlState,
      connected: false,
      paused: false,
      config: null,
      health: null,
      trafficStats: null,
      platformAnalyses: Array.isArray(platformAnalyses) ? platformAnalyses : [],
      platformState,
    };
    return controlState;
  }

  controlState = {
    ...statusData,
    trafficStats,
    platformAnalyses: Array.isArray(platformAnalyses) ? platformAnalyses : [],
    platformState,
  };
  return controlState;
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
  for (const p of ['anthropic-direct', 'claude-code']) {
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

// ── Sektion 6: Traffic Control ────────────────────

function renderTrafficControl(container) {
  const section = el('div', 'control-section');
  section.appendChild(sectionTitle('Traffic Control'));

  const stats = controlState.trafficStats;
  if (!stats) {
    section.appendChild(noDataMsg());
    container.appendChild(section);
    return;
  }

  const items = [
    ['Primary Provider', stats.primary_provider ?? '--'],
    ['Internal Provider', stats.internal_primary_provider ?? stats.primary_provider ?? '--'],
    ['External MITM Provider', stats.external_mitm_provider ?? '--'],
    ['Kosten heute', formatUSD(stats.current_cost_usd)],
    ['Ersparnis heute', formatUSD(stats.estimated_savings_usd)],
    ['Hochrechnung/Tag Kosten', formatUSD(stats.projected_daily_cost_usd)],
    ['Hochrechnung/Tag Ersparnis', formatUSD(stats.projected_daily_savings_usd)],
    ['Avg Forward Cost', formatUSD(stats.avg_forward_cost_usd)],
    ['Forward Calls', formatNum(stats.forward_calls ?? 0)],
    ['Synthesis Count', formatNum(stats.synthesis_count ?? 0)],
    ['Synthesis Rate', formatPercent(stats.synthesis_rate)],
    ['Tick Sync', stats.tick_sync_enabled ? 'Ja' : 'Nein'],
    ['Tick Sync Runtime', stats.tick_sync_runtime_enabled ? 'Ja' : 'Nein'],
    ['Tick Sync Pending', formatNum(stats.tick_sync_pending ?? 0)],
    ['Synthesis aktiv', stats.synthesis_enabled ? 'Ja' : 'Nein'],
    ['Sequencing aktiv', stats.sequencing_enabled ? 'Ja' : 'Nein'],
    ['API-CP aktiv', stats.apicp_enabled ? 'Ja' : 'Nein'],
    ['Active Patterns', formatNum(stats.active_patterns ?? 0)],
    ['Queue Depth', stats.queue_depth == null ? '--' : formatNum(stats.queue_depth)],
    ['Active Forward Calls', stats.active_forward_calls == null ? '--' : formatNum(stats.active_forward_calls)],
    ['Pending Intercepts', formatNum(stats.pending_intercepts ?? 0)],
    ['Pending Response Intercepts', formatNum(stats.pending_response_intercepts ?? 0)],
    ['Response Logs', formatNum(stats.response_log_entries ?? 0)],
    ['Tick Sync Timeout', stats.tick_sync_timeout_ms == null ? '--' : `${stats.tick_sync_timeout_ms} ms`],
    ['P3 Timeout', stats.p3_timeout_ms == null ? '--' : `${stats.p3_timeout_ms} ms`],
    ['Intercept Mode', stats.intercept_mode ?? '--'],
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

  container.appendChild(section);
}

// ── Sektion 7: Live Config ────────────────────────

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

function renderPlatformAnalyses(container) {
  const section = el('div', 'control-section');
  section.id = 'platform-analyses-section';
  section.appendChild(sectionTitle('Platform Analyses'));

  const actionRow = el('div', 'control-row');
  const info = el('span', 'control-value');
  info.textContent = 'Manueller Analyse-Trigger fuer Platform-Controlplane';
  actionRow.appendChild(info);

  const analyzeBtn = el('button', 'control-btn primary');
  analyzeBtn.id = 'platform-analyze-btn';
  analyzeBtn.textContent = 'Analyse jetzt ausloesen';
  analyzeBtn.addEventListener('click', async () => {
    analyzeBtn.disabled = true;
    try {
      const resp = await controlFetch('/api/control/platform-analyze', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...authHeaders() },
        body: JSON.stringify({}),
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({}));
        showFeedback(section, 'Fehler: ' + (err.error || resp.statusText), true);
      } else {
        showFeedback(section, 'Platform-Analyse angestossen');
        await loadControlStatus();
        renderControl();
      }
    } catch (e) {
      showFeedback(section, 'Verbindungsfehler: ' + e.message, true);
    } finally {
      analyzeBtn.disabled = false;
    }
  });
  actionRow.appendChild(analyzeBtn);
  section.appendChild(actionRow);

  const list = el('div', 'control-list');
  list.id = 'platform-analysis-list';
  const analyses = Array.isArray(controlState.platformAnalyses)
    ? controlState.platformAnalyses
    : [];

  if (analyses.length === 0) {
    list.appendChild(noDataMsg('Noch keine Platform-Analysen im Event Store'));
  } else {
    for (const analysis of analyses) {
      const item = el('div', 'control-section compact');
      item.className = 'control-section compact platform-analysis-item';
      item.setAttribute('data-trigger', analysis.trigger || '');
      item.setAttribute('data-severity', analysis.severity || '');
      item.setAttribute('data-suggested-action', analysis.suggested_action || '');

      const header = el('div', 'control-row');
      const summary = el('strong', 'control-value');
      summary.textContent = analysis.summary || '(ohne Summary)';
      header.appendChild(summary);
      const severity = el('span', 'control-value');
      severity.textContent = String(analysis.severity || 'info').toUpperCase();
      header.appendChild(severity);
      item.appendChild(header);

      const meta = [
        ['Trigger', analysis.trigger || '--'],
        ['Target', analysis.target || analysis.aggregate_id || '--'],
        ['Suggested Action', analysis.suggested_action || '--'],
        ['Provider', analysis.provider || '--'],
        ['Model', analysis.model || '--'],
        ['Tick', analysis.tick],
      ];
      for (const [label, value] of meta) {
        const row = el('div', 'control-row compact');
        row.appendChild(labelEl(label + ':'));
        const val = el('span', 'control-value mono');
        val.textContent = String(value);
        row.appendChild(val);
        item.appendChild(row);
      }

      const recommendation = el('div', 'control-value');
      recommendation.textContent = analysis.recommendation || 'Keine Empfehlung';
      item.appendChild(recommendation);

      if (Array.isArray(analysis.unresolved_keys) && analysis.unresolved_keys.length > 0) {
        const unresolved = el('div', 'control-value mono');
        unresolved.textContent = 'Unresolved: ' + analysis.unresolved_keys.join(', ');
        item.appendChild(unresolved);
      }

      list.appendChild(item);
    }
  }

  section.appendChild(list);
  container.appendChild(section);
}

function renderPlatformState(container) {
  const section = el('div', 'control-section');
  section.id = 'platform-state-section';
  section.appendChild(sectionTitle('Platform State'));

  const state = controlState.platformState;
  if (!state) {
    section.appendChild(noDataMsg('Platform-State derzeit nicht verfuegbar'));
    container.appendChild(section);
    return;
  }

  const summaryRows = [
    ['Current Tick', state.current_tick],
    ['LLM Enabled', state.llm_enabled ? 'Ja' : 'Nein'],
    ['Analysis Interval', state.llm_analysis_interval_secs + 's'],
    ['Retry Delay', state.llm_retry_delay_secs + 's'],
    ['Last Analysis Tick', state.last_analysis_tick ?? '--'],
    ['Last Analysis Trigger', state.last_analysis_trigger ?? '--'],
    ['Last Scheduled Analysis', state.last_scheduled_analysis_tick ?? '--'],
    ['Stall Grace Ticks', state.stall_recent_activity_grace_ticks ?? '--'],
  ];
  for (const [label, value] of summaryRows) {
    const row = el('div', 'control-row compact');
    row.appendChild(labelEl(label + ':'));
    const val = el('span', 'control-value mono');
    val.textContent = String(value);
    row.appendChild(val);
    section.appendChild(row);
  }

  const unresolvedTitle = el('div', 'control-subtitle');
  unresolvedTitle.textContent = 'Unresolved Counters';
  section.appendChild(unresolvedTitle);
  const unresolvedBox = el('div', 'control-json');
  unresolvedBox.textContent = JSON.stringify(state.unresolved_counts || {}, null, 2);
  unresolvedBox.id = 'platform-unresolved-counts';
  section.appendChild(unresolvedBox);

  const overrideTitle = el('div', 'control-subtitle');
  overrideTitle.textContent = 'Threshold Overrides';
  section.appendChild(overrideTitle);
  const overrideBox = el('pre', 'control-json');
  overrideBox.id = 'platform-threshold-overrides';
  overrideBox.textContent = JSON.stringify(state.threshold_overrides || {}, null, 2);
  section.appendChild(overrideBox);

  const tableTitle = el('div', 'control-subtitle');
  tableTitle.textContent = 'Agent Runtime State';
  section.appendChild(tableTitle);

  const table = document.createElement('table');
  table.id = 'platform-state-table';
  table.className = 'snapshot-table';
  const thead = document.createElement('thead');
  const headRow = document.createElement('tr');
  ['Agent', 'Aggregate', 'Profile', 'Last Activity Tick', 'cgroup'].forEach((text) => {
    const th = document.createElement('th');
    th.textContent = text;
    headRow.appendChild(th);
  });
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = document.createElement('tbody');
  for (const agent of state.agents || []) {
    const row = document.createElement('tr');
    row.setAttribute('data-platform-agent-id', String(agent.aggregate_id || agent.agent_id));

    const nameCell = document.createElement('td');
    nameCell.textContent = agent.name || '--';
    row.appendChild(nameCell);

    const aggCell = document.createElement('td');
    aggCell.textContent = agent.aggregate_id || '--';
    row.appendChild(aggCell);

    const profileCell = document.createElement('td');
    profileCell.textContent = agent.current_profile || '--';
    row.appendChild(profileCell);

    const activityCell = document.createElement('td');
    activityCell.textContent = String(agent.last_activity_tick ?? '--');
    row.appendChild(activityCell);

    const cgroupCell = document.createElement('td');
    cgroupCell.textContent = agent.cgroup_path || '--';
    row.appendChild(cgroupCell);

    tbody.appendChild(row);
  }
  table.appendChild(tbody);
  section.appendChild(table);

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
    setApiKey(keyInput.value);
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

function noDataMsg(text = 'Gateway nicht verbunden') {
  const d = el('div', 'control-no-data');
  d.textContent = text;
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
  if (n == null) return '--';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K';
  return String(n);
}

function formatUSD(n) {
  if (n == null) return '--';
  return '$' + Number(n).toFixed(2);
}

function formatPercent(n) {
  if (n == null) return '--';
  return (Number(n) * 100).toFixed(1) + '%';
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
  renderTrafficControl(container);
  renderPlatformAnalyses(container);
  renderPlatformState(container);
  renderLiveConfig(container);
  renderSnapshotSection(container);
}

async function initControl() {
  await loadControlStatus();
  renderControl();
}

// ── Time Machine: Snapshot-Sektion ──

async function loadSnapshots() {
  try {
    var res = await fetch('/api/control/snapshots');
    if (res.ok) return await res.json();
  } catch (_) { /* ignore */ }
  return [];
}

function renderSnapshotSection(container) {
  var section = document.createElement('div');
  section.className = 'control-section';
  section.id = 'snapshot-section';

  var h3 = document.createElement('h3');
  h3.textContent = 'Time Machine — World Snapshots';
  section.appendChild(h3);

  var createBtn = document.createElement('button');
  createBtn.className = 'control-btn';
  createBtn.textContent = 'Jetzt Snapshot erstellen';
  createBtn.addEventListener('click', async function() {
    createBtn.disabled = true;
    createBtn.textContent = 'Erstelle...';
    try {
      await fetch('/api/control/snapshot', {
        method: 'POST',
        headers: Object.assign({ 'Content-Type': 'application/json' }, authHeaders()),
        body: '{}',
      });
      await refreshSnapshotList(section);
    } catch (_) { /* ignore */ }
    createBtn.disabled = false;
    createBtn.textContent = 'Jetzt Snapshot erstellen';
  });
  section.appendChild(createBtn);

  var listDiv = document.createElement('div');
  listDiv.id = 'snapshot-list';
  listDiv.textContent = 'Lade Snapshots...';
  section.appendChild(listDiv);

  container.appendChild(section);
  refreshSnapshotList(section);
}

async function refreshSnapshotList(section) {
  var listDiv = section.querySelector('#snapshot-list');
  if (!listDiv) return;
  var snapshots = await loadSnapshots();
  while (listDiv.firstChild) listDiv.removeChild(listDiv.firstChild);

  if (snapshots.length === 0) {
    listDiv.textContent = 'Keine Snapshots vorhanden';
    return;
  }

  var table = document.createElement('table');
  table.className = 'snapshot-table';
  var thead = document.createElement('thead');
  var headerRow = document.createElement('tr');
  ['Tier', 'Tick', 'Sim Hour', 'Groesse', 'Erstellt', 'Aktion'].forEach(function(text) {
    var th = document.createElement('th');
    th.textContent = text;
    headerRow.appendChild(th);
  });
  thead.appendChild(headerRow);
  table.appendChild(thead);

  var tbody = document.createElement('tbody');
  for (var i = 0; i < snapshots.length; i++) {
    var snap = snapshots[i];
    var row = document.createElement('tr');

    var tdTier = document.createElement('td');
    var badge = document.createElement('span');
    badge.className = 'tier-badge tier-' + snap.tier;
    badge.textContent = snap.tier;
    tdTier.appendChild(badge);
    row.appendChild(tdTier);

    var tdTick = document.createElement('td');
    tdTick.textContent = String(snap.tick);
    row.appendChild(tdTick);

    var tdHour = document.createElement('td');
    tdHour.textContent = (snap.sim_hour || 0).toFixed(1) + 'h';
    row.appendChild(tdHour);

    var tdSize = document.createElement('td');
    tdSize.textContent = formatKB(snap.payload_size_bytes);
    row.appendChild(tdSize);

    var tdCreated = document.createElement('td');
    tdCreated.textContent = new Date(snap.created_at_ms).toLocaleString('de-DE');
    row.appendChild(tdCreated);

    var tdAction = document.createElement('td');
    var restoreBtn = document.createElement('button');
    restoreBtn.className = 'control-btn control-btn-small';
    restoreBtn.textContent = 'Restore';
    restoreBtn.setAttribute('data-snapshot-id', snap.id);
    restoreBtn.addEventListener('click', function() {
      var id = this.getAttribute('data-snapshot-id');
      if (confirm('Simulation auf diesen Snapshot zuruecksetzen?\n\nSnapshot ID: ' + id)) {
        fetch('/api/control/restore', {
          method: 'POST',
          headers: Object.assign({ 'Content-Type': 'application/json' }, authHeaders()),
          body: JSON.stringify({ snapshot_id: id }),
        }).then(function() {
          alert('Restore gestartet');
        });
      }
    });
    tdAction.appendChild(restoreBtn);
    row.appendChild(tdAction);

    tbody.appendChild(row);
  }
  table.appendChild(tbody);
  listDiv.appendChild(table);
}

function formatKB(bytes) {
  if (bytes == null) return '--';
  if (bytes > 1048576) return (bytes / 1048576).toFixed(1) + ' MB';
  if (bytes > 1024) return (bytes / 1024).toFixed(0) + ' KB';
  return bytes + ' B';
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
