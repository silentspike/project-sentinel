export function renderMetrics(metrics) {
  const container = document.getElementById('view-metrics');
  while (container.firstChild) container.removeChild(container.firstChild);

  // eBPF Monitoring Mode Badge
  const badge = document.createElement('div');
  badge.id = 'ebpf-mode-badge';
  badge.className = 'ebpf-badge loading';
  badge.textContent = 'eBPF: ...';
  container.appendChild(badge);
  fetchEbpfStatus(badge);

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
    { label: 'Nightrun OK', value: String(metrics.nightrun_consolidated ?? 0), id: 'nightrun-ok', icon: 'memory' },
    { label: 'Nightrun Fail', value: String(metrics.nightrun_failed ?? 0), id: 'nightrun-fail', icon: 'alert' },
    { label: 'Drift-Alerts', value: String(metrics.evolution_drifts ?? 0), id: 'evo-drift', icon: 'evolution' },
    { label: 'Fatigue-Alerts', value: String(metrics.evolution_fatigue ?? 0), id: 'evo-fatigue', icon: 'evolution' },
    { label: 'Tick Duration', value: (metrics.tick_duration_ms ?? 0) + 'ms', id: 'tick-duration', icon: 'uptime' },
    { label: 'Effective Rate', value: (metrics.tick_rate_effective_ms ?? 0) + 'ms', id: 'tick-effective', icon: 'rate' },
    { label: 'PSI CPU', value: ((metrics.psi_cpu ?? 0) * 100).toFixed(1) + '%', id: 'psi-cpu', icon: 'agent' },
    { label: 'PSI IO', value: ((metrics.psi_io ?? 0) * 100).toFixed(1) + '%', id: 'psi-io', icon: 'action' },
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
  renderBenchmarkSnapshot(container);
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

function fetchEbpfStatus(badge, retries) {
  retries = retries || 0;
  fetch('/api/ebpf/metrics')
    .then(function(r) { return r.json(); })
    .then(function(data) {
      if (!data.available) {
        if (retries < 3) {
          setTimeout(function() { fetchEbpfStatus(badge, retries + 1); }, 2000);
          return;
        }
        badge.className = 'ebpf-badge unavailable';
        badge.textContent = 'eBPF: N/A';
        return;
      }
      badge.className = 'ebpf-badge ' + (data.mode === 'kernel' ? 'kernel' : data.mode === 'userspace' ? 'userspace' : 'unavailable');
      badge.textContent = 'eBPF: ' + (data.mode === 'kernel' ? 'Kernel' : data.mode === 'userspace' ? 'Userspace' : 'N/A');

      // Render eBPF detail cards after badge
      renderEbpfCards(badge.parentElement, data);
    })
    .catch(function() {
      if (retries < 3) {
        setTimeout(function() { fetchEbpfStatus(badge, retries + 1); }, 2000);
        return;
      }
      badge.className = 'ebpf-badge unavailable';
      badge.textContent = 'eBPF: N/A';
    });
}

function renderBenchmarkSnapshot(container) {
  const section = document.createElement('section');
  section.className = 'benchmark-panel';
  section.id = 'issue276-benchmarks';

  const heading = document.createElement('h2');
  heading.textContent = 'Tick-Loop Benchmarks #276';
  section.appendChild(heading);

  const meta = document.createElement('div');
  meta.className = 'benchmark-meta';
  meta.textContent = 'Lade Benchmark-Snapshot...';
  section.appendChild(meta);

  const body = document.createElement('div');
  body.className = 'benchmark-body';
  section.appendChild(body);

  container.appendChild(section);

  fetch('/api/metrics/benchmarks')
    .then(function(r) { return r.json(); })
    .then(function(data) {
      renderBenchmarkTable(meta, body, data);
    })
    .catch(function() {
      meta.textContent = 'Benchmark-Snapshot nicht verfuegbar';
    });
}

function renderBenchmarkTable(meta, body, data) {
  while (body.firstChild) body.removeChild(body.firstChild);

  meta.textContent = data.host + ' / ' + data.cpu + ' / ' + data.comparison_scope;

  const table = document.createElement('table');
  table.className = 'benchmark-table';

  const thead = document.createElement('thead');
  const headRow = document.createElement('tr');
  for (const label of ['Pfad', 'Vorher', 'Nachher', 'Delta', 'System']) {
    const th = document.createElement('th');
    th.textContent = label;
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = document.createElement('tbody');
  for (const row of data.results || []) {
    const tr = document.createElement('tr');
    appendBenchmarkCell(tr, row.label + ' (' + row.after_benchmark + ')');
    appendBenchmarkCell(tr, formatNsWithSpread(row.before_ns_per_iter, row.before_stddev_ns_per_iter));
    appendBenchmarkCell(tr, formatNsWithSpread(row.after_ns_per_iter, row.after_stddev_ns_per_iter));
    appendBenchmarkCell(tr, row.improvement_percent.toFixed(2) + '% schneller');
    appendBenchmarkCell(tr, row.system_metrics_summary);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  body.appendChild(table);

  if (Array.isArray(data.notes) && data.notes.length > 0) {
    const notes = document.createElement('ul');
    notes.className = 'benchmark-notes';
    for (const note of data.notes) {
      const item = document.createElement('li');
      item.textContent = note;
      notes.appendChild(item);
    }
    body.appendChild(notes);
  }
}

function appendBenchmarkCell(row, text) {
  const td = document.createElement('td');
  td.textContent = text;
  row.appendChild(td);
}

function formatNs(ns) {
  if (ns == null) return '--';
  if (ns >= 1_000_000) return (ns / 1_000_000).toFixed(2) + ' ms';
  if (ns >= 1_000) return (ns / 1_000).toFixed(2) + ' us';
  return String(ns) + ' ns';
}

function formatNsWithSpread(ns, spread) {
  const base = formatNs(ns);
  if (spread == null) return base;
  return base + ' +/- ' + formatNs(spread);
}

function renderEbpfCards(container, data) {
  // Remove previous eBPF grid if exists
  var prev = container.querySelector('.ebpf-grid');
  if (prev) prev.remove();

  var grid = document.createElement('div');
  grid.className = 'ebpf-grid';

  var ebpfCards = [
    { label: 'Stalled Agents', value: String(data.stalled_count), id: 'ebpf-stalled', warn: data.stalled_count > 0 },
    { label: 'Collection Cycle', value: data.collection_cycle_us + ' \u00b5s', id: 'ebpf-cycle', warn: false },
    { label: 'Ring Buffer Drops', value: String(data.ring_buffer_drops), id: 'ebpf-drops', warn: data.ring_buffer_drops > 0 },
    { label: 'I/O Read', value: formatBytes(data.io_read_bytes), id: 'ebpf-io-read', warn: false },
    { label: 'I/O Write', value: formatBytes(data.io_write_bytes), id: 'ebpf-io-write', warn: false },
    { label: 'Avg PSI Stress', value: (data.avg_stress * 100).toFixed(1) + '%', id: 'ebpf-stress', warn: data.avg_stress > 0.5 },
  ];

  for (var i = 0; i < ebpfCards.length; i++) {
    var c = ebpfCards[i];
    var el = document.createElement('div');
    el.className = 'metric-card ebpf-card' + (c.warn ? ' ebpf-warn' : '');
    el.id = 'metric-' + c.id;

    var val = document.createElement('div');
    val.className = 'value';
    val.textContent = c.value;
    el.appendChild(val);

    var lab = document.createElement('div');
    lab.className = 'label';
    lab.textContent = c.label;
    el.appendChild(lab);

    grid.appendChild(el);
  }

  // Stalled agent details (if any)
  if (data.stalled_agents && data.stalled_agents.length > 0) {
    var detail = document.createElement('div');
    detail.className = 'ebpf-stalled-detail';
    var heading = document.createElement('div');
    heading.className = 'ebpf-stalled-heading';
    heading.textContent = 'Stalled Agents:';
    detail.appendChild(heading);
    for (var j = 0; j < data.stalled_agents.length; j++) {
      var sa = data.stalled_agents[j];
      var line = document.createElement('div');
      line.className = 'ebpf-stalled-agent';
      line.textContent = sa.agent + ' (' + sa.seconds + 's)';
      detail.appendChild(line);
    }
    grid.appendChild(detail);
  }

  container.appendChild(grid);
}

function formatBytes(bytes) {
  if (bytes == null || bytes === 0) return '0 B';
  if (bytes >= 1073741824) return (bytes / 1073741824).toFixed(1) + ' GB';
  if (bytes >= 1048576) return (bytes / 1048576).toFixed(1) + ' MB';
  if (bytes >= 1024) return (bytes / 1024).toFixed(1) + ' KB';
  return bytes + ' B';
}
