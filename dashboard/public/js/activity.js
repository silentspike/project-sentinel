// Activity Feed: Liest direkt aus dem EventStore statt aus Agent-Daten.

const EVENT_TYPE_LABELS = {
  agent_spawned: 'Spawn',
  agent_despawned: 'Despawn',
  agent_action_received: 'Aktion',
  agent_status_changed: 'Status',
  transit_started: 'Transit',
  transit_completed: 'Ankunft',
  chaos_triggered: 'Chaos',
  bio_action_performed: 'Bio',
  shift_transition_completed: 'Schicht',
  nightrun_started: 'Nightrun',
  nightrun_completed: 'Nightrun',
  agent_consolidated: 'Memory',
  agent_consolidation_failed: 'Memory',
};

const EVENT_TYPE_CSS = {
  agent_spawned: 'lifecycle',
  agent_despawned: 'lifecycle',
  agent_action_received: 'action',
  agent_status_changed: 'system',
  transit_started: 'transit',
  transit_completed: 'transit',
  chaos_triggered: 'chaos',
  bio_action_performed: 'bio',
  shift_transition_completed: 'system',
  nightrun_started: 'memory',
  nightrun_completed: 'memory',
  agent_consolidated: 'memory',
  agent_consolidation_failed: 'chaos',
};

let activityData = [];

export async function renderActivity() {
  const container = document.getElementById('view-activity');
  while (container.firstChild) container.removeChild(container.firstChild);

  const wrapper = document.createElement('div');
  wrapper.className = 'activity-container';

  const header = document.createElement('div');
  header.className = 'activity-header';

  const h2 = document.createElement('h2');
  h2.textContent = 'Aktivitaeten';
  header.appendChild(h2);

  const countEl = document.createElement('span');
  countEl.className = 'activity-count';
  countEl.id = 'activity-count';
  header.appendChild(countEl);

  wrapper.appendChild(header);

  const list = document.createElement('div');
  list.className = 'activity-list';
  list.id = 'activity-list';
  wrapper.appendChild(list);
  container.appendChild(wrapper);

  await loadActivityEvents();
}

async function loadActivityEvents() {
  try {
    const res = await fetch('/api/activity?limit=200');
    if (!res.ok) return;
    activityData = await res.json();
    renderActivityList(activityData);
  } catch { /* silently ignore */ }
}

function renderActivityList(events) {
  const list = document.getElementById('activity-list');
  if (!list) return;
  while (list.firstChild) list.removeChild(list.firstChild);

  const countEl = document.getElementById('activity-count');
  if (countEl) countEl.textContent = events.length + ' Events';

  if (events.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'activity-empty';
    empty.textContent = 'Keine Aktivitaeten vorhanden';
    list.appendChild(empty);
    return;
  }

  for (const evt of events) {
    list.appendChild(createEventItem(evt));
  }
}

function createEventItem(evt) {
  const item = document.createElement('div');
  const css = EVENT_TYPE_CSS[evt.event_type] || 'system';
  item.className = 'activity-item activity-' + css;

  // Event-Typ Badge (chaos: show specific type from summary if available)
  const badge = document.createElement('span');
  badge.className = 'activity-badge badge-' + css;
  if (evt.event_type === 'chaos_triggered' && evt.summary) {
    const chaosMatch = evt.summary.match(/^Chaos:\s*(.+)/);
    badge.textContent = chaosMatch ? chaosMatch[1] : 'Chaos';
  } else {
    badge.textContent = EVENT_TYPE_LABELS[evt.event_type] || evt.event_type;
  }
  item.appendChild(badge);

  // Summary
  const summary = document.createElement('span');
  summary.className = 'activity-summary';
  summary.textContent = evt.summary;
  item.appendChild(summary);

  // Detail (optional)
  if (evt.detail) {
    const detail = document.createElement('span');
    detail.className = 'activity-detail';
    detail.textContent = evt.detail;
    item.appendChild(detail);
  }

  // Tick
  const tick = document.createElement('span');
  tick.className = 'activity-tick';
  tick.textContent = 'T' + evt.tick;
  item.appendChild(tick);

  return item;
}

export async function updateActivity() {
  await loadActivityEvents();
}
