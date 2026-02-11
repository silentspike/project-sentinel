export function renderMetrics(metrics) {
  const container = document.getElementById('view-metrics');
  while (container.firstChild) container.removeChild(container.firstChild);

  const grid = document.createElement('div');
  grid.className = 'metrics-grid';

  const cards = [
    { label: 'Tick Rate', value: metrics.tick_rate.toFixed(1) + ' Hz', id: 'tick-rate' },
    { label: 'Aktive Agents', value: String(metrics.agent_count), id: 'agent-count' },
    { label: 'Uptime', value: formatUptime(metrics.uptime), id: 'uptime' },
    { label: 'Nachrichten/min', value: metrics.messages_per_min.toFixed(1), id: 'msg-rate' },
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
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return h + 'h ' + m + 'm';
}
