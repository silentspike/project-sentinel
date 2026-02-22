// Operator Cockpit: Priorisierte Incident-Liste mit Actions/Outcomes/SLO.
// Anti-Metric-Wall Design (AC-N1): Liste statt Card-Grid.
// KEIN innerHTML — nur textContent + DOM API.

const SEVERITY_COLORS = {
  critical: 'var(--danger)',
  high: 'var(--danger)',
  medium: 'var(--warning)',
  low: 'var(--text-secondary)',
};

const SEVERITY_LABELS = {
  critical: 'CRIT',
  high: 'HIGH',
  medium: 'MED',
  low: 'LOW',
};

const STATUS_LABELS = {
  active: 'Aktiv',
  pending: 'Ausstehend',
  resolved: 'Geloest',
  failed: 'Fehlgeschlagen',
};

// ── SLO Status Bar ────────────────────────────────

function renderSloBar(violations, container) {
  container.textContent = '';

  const bar = document.createElement('div');
  bar.className = 'cockpit-slo-bar';

  const sloItems = [
    { name: 'Lag', key: 'Projection Lag' },
    { name: 'Nightrun', key: 'Nightrun Failure-Rate' },
    { name: 'Chaos', key: 'Chaos-Frequenz' },
    { name: 'Despawn', key: 'Despawn-Rate' },
  ];

  for (const item of sloItems) {
    const el = document.createElement('div');
    el.className = 'cockpit-slo-item';

    const label = document.createElement('span');
    label.className = 'cockpit-slo-label';
    label.textContent = item.name + ': ';

    const value = document.createElement('span');
    const violation = violations.find(v => v.name === item.key);
    if (violation) {
      value.textContent = String(violation.current_value) + '/' + String(violation.threshold);
      value.className = 'cockpit-slo-value cockpit-slo-violation';
    } else {
      value.textContent = 'OK';
      value.className = 'cockpit-slo-value cockpit-slo-ok';
    }

    el.appendChild(label);
    el.appendChild(value);
    bar.appendChild(el);
  }

  container.appendChild(bar);
}

// ── Incident Item ─────────────────────────────────

function renderIncidentItem(incident) {
  const item = document.createElement('div');
  item.className = 'cockpit-incident-item';
  item.setAttribute('data-severity', incident.severity);
  item.setAttribute('data-status', incident.status);

  // Header line: severity badge + type + summary
  const header = document.createElement('div');
  header.className = 'cockpit-incident-header';

  const badge = document.createElement('span');
  badge.className = 'cockpit-severity-badge';
  badge.textContent = SEVERITY_LABELS[incident.severity] || incident.severity;
  badge.style.color = SEVERITY_COLORS[incident.severity] || 'var(--text-secondary)';
  header.appendChild(badge);

  const summary = document.createElement('span');
  summary.className = 'cockpit-incident-summary';
  summary.textContent = incident.summary;
  header.appendChild(summary);

  const status = document.createElement('span');
  status.className = 'cockpit-incident-status cockpit-status-' + incident.status;
  status.textContent = STATUS_LABELS[incident.status] || incident.status;
  header.appendChild(status);

  item.appendChild(header);

  // Meta line: tick + timestamp
  const meta = document.createElement('div');
  meta.className = 'cockpit-incident-meta';
  const date = new Date(incident.timestamp_ms);
  meta.textContent = 'Tick ' + incident.tick + ' — ' + date.toLocaleString('de-DE');
  if (incident.agent_id) {
    meta.textContent += ' — ' + incident.agent_id;
  }
  if (incident.room_id) {
    meta.textContent += ' — ' + incident.room_id;
  }
  item.appendChild(meta);

  // Actions (AC-3)
  const actionsContainer = document.createElement('div');
  actionsContainer.className = 'cockpit-actions';

  if (incident.actions.length > 0) {
    for (const action of incident.actions) {
      const actionEl = document.createElement('div');
      actionEl.className = 'cockpit-action-item';
      actionEl.textContent = 'Aktion: ' + action.summary;
      actionsContainer.appendChild(actionEl);
    }
  } else {
    const noActions = document.createElement('div');
    noActions.className = 'cockpit-action-empty';
    noActions.textContent = 'Keine Massnahmen eingeleitet';
    actionsContainer.appendChild(noActions);
  }

  item.appendChild(actionsContainer);

  // Outcome (AC-4)
  if (incident.outcome != null) {
    const outcomeEl = document.createElement('div');
    outcomeEl.className = 'cockpit-outcome';
    outcomeEl.textContent = 'Outcome: ' + incident.outcome;
    item.appendChild(outcomeEl);
  } else if (incident.status === 'pending') {
    const pendingEl = document.createElement('div');
    pendingEl.className = 'cockpit-outcome cockpit-outcome-pending';
    pendingEl.textContent = 'Outcome: ausstehend';
    item.appendChild(pendingEl);
  }

  return item;
}

// ── Incident List ─────────────────────────────────

function renderIncidentList(incidents, container) {
  container.textContent = '';

  const active = incidents.filter(i => i.status === 'active' || i.status === 'pending');
  const resolved = incidents.filter(i => i.status === 'resolved' || i.status === 'failed');

  // Active incidents section
  const activeHeader = document.createElement('h3');
  activeHeader.className = 'cockpit-section-header';
  activeHeader.textContent = 'Aktive Incidents (' + active.length + ')';
  container.appendChild(activeHeader);

  if (active.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'cockpit-empty';
    empty.textContent = 'Keine aktiven Incidents';
    container.appendChild(empty);
  } else {
    const activeList = document.createElement('div');
    activeList.className = 'cockpit-incident-list';
    for (const incident of active) {
      activeList.appendChild(renderIncidentItem(incident));
    }
    container.appendChild(activeList);
  }

  // Resolved section (collapsed by default)
  if (resolved.length > 0) {
    const resolvedHeader = document.createElement('h3');
    resolvedHeader.className = 'cockpit-section-header cockpit-resolved-header';
    resolvedHeader.textContent = 'Abgeschlossen (24h): ' + resolved.length;
    resolvedHeader.style.cursor = 'pointer';

    const resolvedList = document.createElement('div');
    resolvedList.className = 'cockpit-incident-list cockpit-resolved-list';
    resolvedList.style.display = 'none';

    resolvedHeader.addEventListener('click', () => {
      const isVisible = resolvedList.style.display !== 'none';
      resolvedList.style.display = isVisible ? 'none' : 'block';
      resolvedHeader.textContent = (isVisible ? 'Abgeschlossen (24h): ' : 'Abgeschlossen (24h): ') + resolved.length;
    });

    for (const incident of resolved) {
      resolvedList.appendChild(renderIncidentItem(incident));
    }

    container.appendChild(resolvedHeader);
    container.appendChild(resolvedList);
  }
}

// ── Main Render ───────────────────────────────────

export function renderCockpit(data) {
  const container = document.getElementById('view-cockpit');
  if (!container) return;

  container.textContent = '';

  // SLO Bar
  const sloContainer = document.createElement('div');
  sloContainer.className = 'cockpit-slo-container';
  renderSloBar(data.slo_violations, sloContainer);
  container.appendChild(sloContainer);

  // Summary line
  const summaryEl = document.createElement('div');
  summaryEl.className = 'cockpit-summary';
  summaryEl.textContent = data.total_active + ' aktiv / ' + data.total_resolved_24h + ' abgeschlossen (24h)';
  container.appendChild(summaryEl);

  // Incident List
  const listContainer = document.createElement('div');
  listContainer.className = 'cockpit-list-container';
  renderIncidentList(data.incidents, listContainer);
  container.appendChild(listContainer);
}

// ── Update (fetches fresh data) ───────────────────

export async function updateCockpit() {
  try {
    const res = await fetch('/api/cockpit');
    const data = await res.json();
    renderCockpit(data);
  } catch {
    // Fetch failed — skip update
  }
}
