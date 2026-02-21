// Chaos Event Live-Feed: Zeigt chaos_triggered Events aus dem EventStore.
// Scrollbare Liste mit Typ, Raum, Beschreibung und Zeitstempel.

export function renderChaos(events) {
  const container = document.getElementById('view-chaos');
  while (container.firstChild) container.removeChild(container.firstChild);

  const wrapper = document.createElement('div');
  wrapper.className = 'chaos-container';

  const header = document.createElement('div');
  header.className = 'chaos-header';

  const h2 = document.createElement('h2');
  h2.textContent = 'Chaos Event Feed';
  header.appendChild(h2);

  const count = document.createElement('span');
  count.className = 'chaos-count';
  count.textContent = events.length + ' Events';
  header.appendChild(count);

  wrapper.appendChild(header);

  const list = document.createElement('div');
  list.className = 'chaos-list';
  list.id = 'chaos-list';

  for (const evt of events) {
    list.appendChild(createChaosItem(evt));
  }

  if (events.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'chaos-empty';
    empty.textContent = 'Keine Chaos-Events vorhanden';
    list.appendChild(empty);
  }

  wrapper.appendChild(list);
  container.appendChild(wrapper);
}

function createChaosItem(evt) {
  const item = document.createElement('div');
  item.className = 'chaos-item';

  // Type badge
  const badge = document.createElement('span');
  badge.className = 'chaos-type-badge';
  badge.textContent = formatChaosType(evt.chaos_type);
  item.appendChild(badge);

  // Room
  if (evt.room_id) {
    const room = document.createElement('span');
    room.className = 'chaos-room';
    room.textContent = evt.room_id;
    item.appendChild(room);
  }

  // Description
  if (evt.description) {
    const desc = document.createElement('div');
    desc.className = 'chaos-description';
    desc.textContent = evt.description;
    item.appendChild(desc);
  }

  // Timestamp + Tick
  const meta = document.createElement('div');
  meta.className = 'chaos-meta';
  const date = new Date(evt.timestamp_ms);
  meta.textContent = 'Tick ' + evt.tick + ' — ' + date.toLocaleString('de-DE');
  item.appendChild(meta);

  return item;
}

function formatChaosType(type) {
  if (!type || type === 'unknown') return 'CHAOS';
  // Convert PascalCase/snake_case to readable
  return type
    .replace(/([A-Z])/g, ' $1')
    .replace(/_/g, ' ')
    .trim()
    .toUpperCase();
}

export async function updateChaos() {
  try {
    const res = await fetch('/api/chaos?limit=100');
    const events = await res.json();
    renderChaos(events);
  } catch {
    // Fetch failed — skip
  }
}
