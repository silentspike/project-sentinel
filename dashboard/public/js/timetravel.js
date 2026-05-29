// Zeitreise-View (Time-Travel Debugging UI, #384)
// Visuelle Snapshot-Navigation entlang der Zeitachse + Welt-Zustand-Preview +
// Hot-Swap-Restore via bestehende Operator-API.
// KEIN innerHTML — ausschliesslich textContent + DOM-API.

let snapshots = [];
let selectedId = null;
let loadError = false;

const TIER_ORDER = ['live', 'hourly', 'daily', 'weekly', 'monthly'];

// ── API-Key (geteilt mit Control-Tab via sessionStorage) ──

function ttApiKey() {
  return sessionStorage.getItem('sentinel_api_key') || '';
}

function ttAuthHeaders() {
  const key = ttApiKey();
  return key ? { 'Authorization': 'Bearer ' + key } : {};
}

// ── Helpers ──

function ttEl(tag, className) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  return e;
}

function formatBytes(bytes) {
  if (bytes == null) return '--';
  if (bytes > 1048576) return (bytes / 1048576).toFixed(1) + ' MB';
  if (bytes > 1024) return (bytes / 1024).toFixed(0) + ' KB';
  return bytes + ' B';
}

function formatTime(ms) {
  if (ms == null) return '--';
  return new Date(ms).toLocaleString('de-DE');
}

// ── Daten laden ──

async function loadSnapshotList() {
  try {
    const res = await fetch('/api/control/snapshots');
    if (!res.ok) {
      loadError = true;
      return [];
    }
    const data = await res.json();
    loadError = false;
    return Array.isArray(data) ? data : [];
  } catch (_) {
    loadError = true;
    return [];
  }
}

async function loadSnapshotState(snapshotId) {
  const res = await fetch(
    '/api/control/snapshot-state?snapshot_id=' + encodeURIComponent(snapshotId),
  );
  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(err.error || ('HTTP ' + res.status));
  }
  return res.json();
}

// ── AC-1: Visuelle Zeitachse ──

function renderTimeline(container) {
  const wrap = ttEl('div', 'tt-timeline-wrap');

  if (snapshots.length === 0) {
    const empty = ttEl('div', 'tt-empty');
    empty.textContent = loadError
      ? 'Snapshots konnten nicht geladen werden (Operator-API nicht erreichbar)'
      : 'Keine Snapshots vorhanden';
    wrap.appendChild(empty);
    container.appendChild(wrap);
    return;
  }

  // Sortiert nach Zeit aufsteigend fuer Achsen-Positionierung
  const sorted = snapshots.slice().sort((a, b) => a.created_at_ms - b.created_at_ms);
  const minT = sorted[0].created_at_ms;
  const maxT = sorted[sorted.length - 1].created_at_ms;
  const span = maxT - minT;

  // Achsen-Beschriftung
  const axisLabels = ttEl('div', 'tt-axis-labels');
  const leftLabel = ttEl('span', 'tt-axis-label');
  leftLabel.textContent = formatTime(minT);
  const rightLabel = ttEl('span', 'tt-axis-label');
  rightLabel.textContent = formatTime(maxT);
  axisLabels.appendChild(leftLabel);
  axisLabels.appendChild(rightLabel);
  wrap.appendChild(axisLabels);

  // Rail mit Markern
  const rail = ttEl('div', 'tt-rail');
  sorted.forEach((snap, idx) => {
    const pct = span > 0
      ? ((snap.created_at_ms - minT) / span) * 100
      : (sorted.length > 1 ? (idx / (sorted.length - 1)) * 100 : 50);
    const marker = ttEl('button', 'tt-marker tier-' + snap.tier);
    marker.style.left = 'calc(' + pct.toFixed(2) + '% - 7px)';
    marker.setAttribute('data-snapshot-id', snap.id);
    marker.setAttribute('aria-label',
      snap.tier + ' Snapshot, Tick ' + snap.tick + ', ' + formatTime(snap.created_at_ms));
    marker.title = snap.tier + ' | Tick ' + snap.tick + ' | ' + formatTime(snap.created_at_ms);
    if (snap.id === selectedId) marker.classList.add('selected');
    marker.addEventListener('click', () => selectSnapshot(snap.id));
    rail.appendChild(marker);
  });
  wrap.appendChild(rail);

  // Tier-Legende
  const legend = ttEl('div', 'tt-legend');
  TIER_ORDER.forEach((tier) => {
    if (!snapshots.some((s) => s.tier === tier)) return;
    const item = ttEl('span', 'tt-legend-item');
    const dot = ttEl('span', 'tt-legend-dot tier-' + tier);
    const txt = ttEl('span', 'tt-legend-text');
    txt.textContent = tier;
    item.appendChild(dot);
    item.appendChild(txt);
    legend.appendChild(item);
  });
  wrap.appendChild(legend);

  container.appendChild(wrap);
}

// ── AC-1: Snapshot-Liste (chronologisch, auswaehlbar) ──

function renderSnapshotList(container) {
  if (snapshots.length === 0) return;

  const section = ttEl('div', 'tt-list-section');
  const title = ttEl('h3', 'tt-subtitle');
  title.textContent = 'Snapshots (' + snapshots.length + ')';
  section.appendChild(title);

  const table = ttEl('table', 'snapshot-table');
  const thead = ttEl('thead');
  const headRow = ttEl('tr');
  ['Tier', 'Zeitstempel', 'Tick', 'Sim Hour', 'Groesse'].forEach((text) => {
    const th = ttEl('th');
    th.textContent = text;
    headRow.appendChild(th);
  });
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = ttEl('tbody');
  // Neueste zuerst
  const sorted = snapshots.slice().sort((a, b) => b.created_at_ms - a.created_at_ms);
  for (const snap of sorted) {
    const row = ttEl('tr', 'tt-list-row');
    row.setAttribute('data-snapshot-id', snap.id);
    if (snap.id === selectedId) row.classList.add('selected');
    row.addEventListener('click', () => selectSnapshot(snap.id));

    const tdTier = ttEl('td');
    const badge = ttEl('span', 'tier-badge tier-' + snap.tier);
    badge.textContent = snap.tier;
    tdTier.appendChild(badge);
    row.appendChild(tdTier);

    const tdTime = ttEl('td');
    tdTime.textContent = formatTime(snap.created_at_ms);
    row.appendChild(tdTime);

    const tdTick = ttEl('td');
    tdTick.textContent = String(snap.tick);
    row.appendChild(tdTick);

    const tdHour = ttEl('td');
    tdHour.textContent = (snap.sim_hour || 0).toFixed(1) + 'h';
    row.appendChild(tdHour);

    const tdSize = ttEl('td');
    tdSize.textContent = formatBytes(snap.payload_size_bytes);
    row.appendChild(tdSize);

    tbody.appendChild(row);
  }
  table.appendChild(tbody);
  section.appendChild(table);
  container.appendChild(section);
}

// ── AC-2 + AC-3: Detail-Panel mit Welt-Zustand + Restore ──

async function renderDetailPanel(container) {
  const panel = ttEl('div', 'tt-detail-panel');
  panel.id = 'tt-detail-panel';

  if (!selectedId) {
    const hint = ttEl('div', 'tt-detail-hint');
    hint.textContent = 'Snapshot auf der Zeitachse oder in der Liste auswaehlen, um den Welt-Zustand zu diesem Zeitpunkt anzuzeigen.';
    panel.appendChild(hint);
    container.appendChild(panel);
    return;
  }

  const snap = snapshots.find((s) => s.id === selectedId);
  const title = ttEl('h3', 'tt-subtitle');
  title.textContent = 'Welt-Zustand zum Snapshot-Zeitpunkt';
  panel.appendChild(title);

  const loading = ttEl('div', 'tt-loading');
  loading.textContent = 'Lade Welt-Zustand...';
  panel.appendChild(loading);
  container.appendChild(panel);

  let state;
  try {
    state = await loadSnapshotState(selectedId);
  } catch (e) {
    loading.textContent = 'Fehler: ' + e.message;
    loading.className = 'tt-error';
    return;
  }

  // Nur rendern, wenn die Auswahl waehrenddessen nicht gewechselt hat
  if (selectedId !== state.snapshot_id) return;
  panel.removeChild(loading);

  // Kopfzeile: Tier + Zeit
  const header = ttEl('div', 'tt-detail-header');
  const badge = ttEl('span', 'tier-badge tier-' + state.tier);
  badge.textContent = state.tier;
  header.appendChild(badge);
  const when = ttEl('span', 'tt-detail-time');
  when.textContent = formatTime(state.created_at_ms);
  header.appendChild(when);
  panel.appendChild(header);

  // Kennzahlen
  const metaRows = [
    ['Tick', String(state.tick)],
    ['Sim Hour', (state.sim_hour || 0).toFixed(2) + 'h'],
    ['Last Event ID', String(state.last_event_id)],
    ['Groesse', formatBytes(snap ? snap.payload_size_bytes : null)],
  ];
  const metaBox = ttEl('div', 'tt-meta-box');
  for (const [label, value] of metaRows) {
    const r = ttEl('div', 'tt-meta-row');
    const l = ttEl('span', 'tt-meta-label');
    l.textContent = label + ':';
    const v = ttEl('span', 'tt-meta-value');
    v.textContent = value;
    r.appendChild(l);
    r.appendChild(v);
    metaBox.appendChild(r);
  }
  panel.appendChild(metaBox);

  // Welt-Zustand-Counts (AC-2): aktive Agents (authoritative) + belegte Raeume
  const stats = ttEl('div', 'tt-stats-row');

  const agentStat = ttEl('div', 'tt-stat');
  const agentNum = ttEl('div', 'tt-stat-num');
  agentNum.textContent = state.active_agent_count != null
    ? String(state.active_agent_count)
    : String(state.present_agent_count);
  const agentLbl = ttEl('div', 'tt-stat-lbl');
  agentLbl.textContent = state.active_agent_count != null ? 'aktive Agents' : 'Agents';
  agentStat.appendChild(agentNum);
  agentStat.appendChild(agentLbl);
  stats.appendChild(agentStat);

  const presentStat = ttEl('div', 'tt-stat');
  const presentNum = ttEl('div', 'tt-stat-num');
  presentNum.textContent = String(state.present_agent_count);
  const presentLbl = ttEl('div', 'tt-stat-lbl');
  presentLbl.textContent = 'im Gebaeude';
  presentStat.appendChild(presentNum);
  presentStat.appendChild(presentLbl);
  stats.appendChild(presentStat);

  const roomStat = ttEl('div', 'tt-stat');
  const roomNum = ttEl('div', 'tt-stat-num');
  roomNum.textContent = String(state.room_count);
  const roomLbl = ttEl('div', 'tt-stat-lbl');
  roomLbl.textContent = 'belegte Raeume';
  roomStat.appendChild(roomNum);
  roomStat.appendChild(roomLbl);
  stats.appendChild(roomStat);
  panel.appendChild(stats);

  const note = ttEl('div', 'tt-note');
  note.textContent = '"aktive Agents" = schicht-aktive Agents zu diesem Tick (tick_snapshot, ' +
    'deckt sich mit der Live-Ansicht). "im Gebaeude" = alle anwesenden Agents inkl. ' +
    'Off-Shift, abgeleitet aus den Lifecycle-Events bis zur Snapshot-Grenze.';
  panel.appendChild(note);

  // Pro-Raum-Belegung der anwesenden Agents
  if (state.rooms && state.rooms.length > 0) {
    const roomTitle = ttEl('div', 'tt-room-title');
    roomTitle.textContent = 'Raum-Belegung (anwesende Agents)';
    panel.appendChild(roomTitle);

    const roomTable = ttEl('table', 'snapshot-table');
    const rt = ttEl('tbody');
    for (const room of state.rooms) {
      const tr = ttEl('tr');
      const tdName = ttEl('td');
      tdName.textContent = room.name;
      const tdCount = ttEl('td');
      tdCount.textContent = String(room.occupant_count);
      tr.appendChild(tdName);
      tr.appendChild(tdCount);
      rt.appendChild(tr);
    }
    roomTable.appendChild(rt);
    panel.appendChild(roomTable);
  }

  // AC-3: Restore-Button mit Confirm-Dialog
  const restoreBtn = ttEl('button', 'control-btn tt-restore-btn');
  restoreBtn.textContent = 'Auf diesen Snapshot zuruecksetzen (Restore)';
  restoreBtn.setAttribute('data-snapshot-id', state.snapshot_id);
  restoreBtn.addEventListener('click', () => triggerRestore(state, panel, restoreBtn));
  panel.appendChild(restoreBtn);

  const feedbackSlot = ttEl('div', 'tt-feedback-slot');
  feedbackSlot.id = 'tt-feedback-slot';
  panel.appendChild(feedbackSlot);
}

async function triggerRestore(state, panel, btn) {
  const ok = confirm(
    'Hot-Swap-Restore ausfuehren?\n\n' +
    'Die laufende Simulation wird auf diesen Snapshot zurueckgesetzt:\n' +
    'Tier: ' + state.tier + '\n' +
    'Tick: ' + state.tick + '\n' +
    'Zeit: ' + formatTime(state.created_at_ms) + '\n' +
    'Aktive Agents: ' + (state.active_agent_count != null ? state.active_agent_count : state.present_agent_count) +
    ' | belegte Raeume: ' + state.room_count + '\n\n' +
    'Snapshot-ID: ' + state.snapshot_id,
  );
  if (!ok) return;

  btn.disabled = true;
  btn.textContent = 'Restore laeuft...';
  const slot = panel.querySelector('#tt-feedback-slot');
  try {
    const res = await fetch('/api/control/restore', {
      method: 'POST',
      headers: Object.assign({ 'Content-Type': 'application/json' }, ttAuthHeaders()),
      body: JSON.stringify({ snapshot_id: state.snapshot_id }),
    });
    if (res.ok) {
      if (slot) {
        slot.textContent = 'Restore erfolgreich ausgeloest — Dashboard aktualisiert sich live.';
        slot.className = 'tt-feedback-slot success';
      }
    } else {
      const err = await res.json().catch(() => ({}));
      const msg = res.status === 401
        ? 'Nicht autorisiert — API-Key im Control-Tab oder unten eingeben.'
        : ('Fehler: ' + (err.error || res.statusText));
      if (slot) {
        slot.textContent = msg;
        slot.className = 'tt-feedback-slot error';
      }
    }
  } catch (e) {
    if (slot) {
      slot.textContent = 'Verbindungsfehler: ' + e.message;
      slot.className = 'tt-feedback-slot error';
    }
  }
  btn.disabled = false;
  btn.textContent = 'Auf diesen Snapshot zuruecksetzen (Restore)';
}

// ── Auswahl wechseln ──

function selectSnapshot(id) {
  selectedId = id;
  renderTimeTravel();
}

// ── Aktionsleiste (API-Key + Snapshot erstellen) ──

function renderActionBar(container) {
  const bar = ttEl('div', 'tt-actionbar');

  const keyInput = document.createElement('input');
  keyInput.type = 'password';
  keyInput.id = 'tt-api-key-input';
  keyInput.className = 'control-input';
  keyInput.placeholder = 'API-Key (fuer Restore)';
  keyInput.value = ttApiKey();
  keyInput.addEventListener('change', () => {
    sessionStorage.setItem('sentinel_api_key', keyInput.value);
  });
  bar.appendChild(keyInput);

  const createBtn = ttEl('button', 'control-btn');
  createBtn.id = 'tt-create-btn';
  createBtn.textContent = 'Jetzt Snapshot erstellen';
  createBtn.addEventListener('click', async () => {
    createBtn.disabled = true;
    createBtn.textContent = 'Erstelle...';
    try {
      await fetch('/api/control/snapshot', {
        method: 'POST',
        headers: Object.assign({ 'Content-Type': 'application/json' }, ttAuthHeaders()),
        body: '{}',
      });
      await refreshTimeTravel();
    } catch (_) { /* ignore */ }
    createBtn.disabled = false;
    createBtn.textContent = 'Jetzt Snapshot erstellen';
  });
  bar.appendChild(createBtn);

  const refreshBtn = ttEl('button', 'control-btn');
  refreshBtn.id = 'tt-refresh-btn';
  refreshBtn.textContent = 'Aktualisieren';
  refreshBtn.addEventListener('click', refreshTimeTravel);
  bar.appendChild(refreshBtn);

  container.appendChild(bar);
}

// ── Haupt-Render ──

function renderTimeTravel() {
  const container = document.getElementById('view-timetravel');
  if (!container) return;
  container.textContent = '';

  const title = ttEl('h2', 'tt-title');
  title.textContent = 'Time Machine — Zeitreise & Restore';
  container.appendChild(title);

  renderActionBar(container);
  renderTimeline(container);

  const split = ttEl('div', 'tt-split');
  const listCol = ttEl('div', 'tt-list-col');
  renderSnapshotList(listCol);
  split.appendChild(listCol);

  const detailCol = ttEl('div', 'tt-detail-col');
  renderDetailPanel(detailCol);
  split.appendChild(detailCol);

  container.appendChild(split);
}

async function refreshTimeTravel() {
  snapshots = await loadSnapshotList();
  // Auswahl beibehalten, falls Snapshot noch existiert
  if (selectedId && !snapshots.some((s) => s.id === selectedId)) {
    selectedId = null;
  }
  renderTimeTravel();
}

async function initTimeTravel() {
  await refreshTimeTravel();
}

export { initTimeTravel, refreshTimeTravel };
