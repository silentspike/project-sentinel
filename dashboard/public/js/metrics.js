export function renderMetrics(metrics) {
  const container = document.getElementById('view-metrics');
  while (container.firstChild) container.removeChild(container.firstChild);

  const grid = document.createElement('div');
  grid.className = 'metrics-grid';

  const cards = [
    { label: 'Aktive Agents', value: String(metrics.active_agents), id: 'active-agents' },
    { label: 'Aktionen', value: String(metrics.total_actions), id: 'total-actions' },
    { label: 'Transits', value: String(metrics.total_transits), id: 'total-transits' },
    { label: 'Chaos Events', value: String(metrics.chaos_events), id: 'chaos-events' },
    { label: 'Schichtwechsel', value: String(metrics.shift_changes), id: 'shift-changes' },
    { label: 'Uptime', value: formatUptime(metrics.uptime), id: 'uptime' },
  ];

  for (const card of cards) {
    const el = document.createElement('div');
    el.className = 'metric-card';
    el.id = 'metric-' + card.id;

    const value = document.createElement('div');
    value.className = 'value';
    value.textContent = card.value;
    el.appendChild(value);

    const label = document.createElement('div');
    label.className = 'label';
    label.textContent = card.label;
    el.appendChild(label);

    grid.appendChild(el);
  }

  container.appendChild(grid);
}

function formatUptime(seconds) {
  if (seconds == null) return '--';
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return h + 'h ' + m + 'm';
}
