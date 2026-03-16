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
  bio_state_updated: 'Bio',
  room_physics_updated: 'Physik',
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
  bio_state_updated: 'bio',
  room_physics_updated: 'physics',
  shift_transition_completed: 'system',
  nightrun_started: 'memory',
  nightrun_completed: 'memory',
  agent_consolidated: 'memory',
  agent_consolidation_failed: 'chaos',
};

const FILTER_OPTIONS = [
  { key: 'all', label: 'Alle', types: null },
  {
    key: 'focus',
    label: 'Reaktionen',
    types: [
      'agent_action_received',
      'chaos_triggered',
      'transit_started',
      'transit_completed',
      'bio_action_performed',
    ],
  },
  { key: 'actions', label: 'Aktionen', types: ['agent_action_received'] },
  { key: 'chaos', label: 'Chaos', types: ['chaos_triggered'] },
  {
    key: 'transit',
    label: 'Transit',
    types: ['transit_started', 'transit_completed'],
  },
  { key: 'physics', label: 'Physik', types: ['room_physics_updated'] },
  {
    key: 'bio',
    label: 'Bio',
    types: ['bio_state_updated', 'bio_action_performed'],
  },
];

let activityData = [];
let activityFilterState = {
  mode: 'all',
  query: '',
};

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
  wrapper.appendChild(createActivityControls());

  const list = document.createElement('div');
  list.className = 'activity-list';
  list.id = 'activity-list';
  wrapper.appendChild(list);
  container.appendChild(wrapper);

  await loadActivityEvents();
}

function createActivityControls() {
  const controls = document.createElement('div');
  controls.className = 'activity-controls';

  const filterGroup = document.createElement('div');
  filterGroup.className = 'activity-filter-group';
  for (const filter of FILTER_OPTIONS) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className =
      'activity-filter-btn' +
      (activityFilterState.mode === filter.key ? ' active' : '');
    button.textContent = filter.label;
    button.dataset.filterKey = filter.key;
    button.addEventListener('click', () => {
      activityFilterState.mode = filter.key;
      syncActivityControls();
      renderActivityList(activityData);
    });
    filterGroup.appendChild(button);
  }
  controls.appendChild(filterGroup);

  const search = document.createElement('input');
  search.type = 'search';
  search.className = 'activity-search';
  search.id = 'activity-search';
  search.placeholder = 'Agent, Raum oder Text filtern';
  search.value = activityFilterState.query;
  search.addEventListener('input', () => {
    activityFilterState.query = search.value;
    renderActivityList(activityData);
  });
  controls.appendChild(search);

  return controls;
}

function syncActivityControls() {
  const buttons = document.querySelectorAll('.activity-filter-btn');
  for (const button of buttons) {
    const isActive = button.dataset.filterKey === activityFilterState.mode;
    button.classList.toggle('active', isActive);
  }

  const search = document.getElementById('activity-search');
  if (search) {
    search.value = activityFilterState.query;
  }
}

async function loadActivityEvents() {
  try {
    const res = await fetch('/api/activity?limit=200');
    if (!res.ok) return;
    activityData = await res.json();
    renderActivityList(activityData);
  } catch {
    /* silently ignore */
  }
}

function getFilteredEvents(events) {
  const selectedFilter = FILTER_OPTIONS.find(
    (entry) => entry.key === activityFilterState.mode,
  );
  const normalizedQuery = activityFilterState.query.trim().toLowerCase();

  return events.filter((evt) => {
    if (selectedFilter && Array.isArray(selectedFilter.types)) {
      if (!selectedFilter.types.includes(evt.event_type)) {
        return false;
      }
    }

    if (!normalizedQuery) return true;
    const haystack = [
      evt.summary,
      evt.detail,
      evt.room,
      EVENT_TYPE_LABELS[evt.event_type],
      evt.event_type,
    ]
      .filter(Boolean)
      .join(' ')
      .toLowerCase();
    return haystack.includes(normalizedQuery);
  });
}

function renderActivityList(events) {
  const list = document.getElementById('activity-list');
  if (!list) return;
  while (list.firstChild) list.removeChild(list.firstChild);

  const filteredEvents = getFilteredEvents(events);
  const countEl = document.getElementById('activity-count');
  if (countEl) {
    countEl.textContent =
      filteredEvents.length === events.length
        ? events.length + ' Events'
        : filteredEvents.length + ' / ' + events.length + ' Events';
  }

  if (filteredEvents.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'activity-empty';
    empty.textContent =
      events.length === 0
        ? 'Keine Aktivitaeten vorhanden'
        : 'Keine passenden Aktivitaeten fuer den aktuellen Filter';
    list.appendChild(empty);
    return;
  }

  for (const evt of filteredEvents) {
    list.appendChild(createEventItem(evt));
  }
}

function createEventItem(evt) {
  const item = document.createElement('div');
  const css = EVENT_TYPE_CSS[evt.event_type] || 'system';
  item.className = 'activity-item activity-' + css;

  const badge = document.createElement('span');
  badge.className = 'activity-badge badge-' + css;
  if (evt.event_type === 'chaos_triggered' && evt.summary) {
    const chaosMatch = evt.summary.match(/^Chaos:\s*(.+)/);
    badge.textContent = chaosMatch ? chaosMatch[1] : 'Chaos';
  } else {
    badge.textContent = EVENT_TYPE_LABELS[evt.event_type] || evt.event_type;
  }
  item.appendChild(badge);

  const summary = document.createElement('span');
  summary.className = 'activity-summary';
  summary.textContent = evt.summary;
  item.appendChild(summary);

  if (evt.detail) {
    const detail = document.createElement('span');
    detail.className = 'activity-detail';
    detail.textContent = evt.detail;
    item.appendChild(detail);
  }

  if (evt.room) {
    const room = document.createElement('span');
    room.className = 'activity-room';
    room.textContent = evt.room;
    item.appendChild(room);
  }

  const tick = document.createElement('span');
  tick.className = 'activity-tick';
  tick.textContent = 'T' + evt.tick;
  item.appendChild(tick);

  return item;
}

export async function updateActivity() {
  await loadActivityEvents();
}
