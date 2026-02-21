export function renderMetrics(metrics) {
  const container = document.getElementById('view-metrics');
  while (container.firstChild) container.removeChild(container.firstChild);

  const grid = document.createElement('div');
  grid.className = 'metrics-grid';

  const cards = [
    { label: 'Aktive Agents', value: String(metrics.active_agents), id: 'active-agents', icon: 'agent' },
    { label: 'Aktionen', value: String(metrics.total_actions), id: 'total-actions', icon: 'action' },
    { label: 'Transits', value: String(metrics.total_transits), id: 'total-transits', icon: 'transit' },
    { label: 'Chaos Events', value: String(metrics.chaos_events), id: 'chaos-events', icon: 'chaos' },
    { label: 'Schichtwechsel', value: String(metrics.shift_changes), id: 'shift-changes', icon: 'shift' },
    { label: 'Uptime', value: formatUptime(metrics.uptime), id: 'uptime', icon: 'uptime' },
    { label: 'Events Gesamt', value: formatNumber(metrics.total_events), id: 'total-events', icon: 'event' },
    { label: 'Events/Min', value: String(metrics.event_rate_per_min ?? 0), id: 'event-rate', icon: 'rate' },
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

function formatNumber(n) {
  if (n == null) return '0';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k';
  return String(n);
}
