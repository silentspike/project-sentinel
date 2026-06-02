// Router und WebSocket Manager
import { initControl } from './control.js';
import { initTimeTravel, refreshTimeTravel } from './timetravel.js';
import { refreshAuthStatus } from './auth.js';

let ws = null;

function initNavigation() {
  const nav = document.getElementById('main-nav');
  const buttons = nav.querySelectorAll('.nav-btn');

  buttons.forEach(btn => {
    btn.addEventListener('click', () => {
      buttons.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');

      document.querySelectorAll('.view').forEach(v => v.classList.remove('active'));
      const viewName = btn.getAttribute('data-view');
      const viewId = 'view-' + viewName;
      const view = document.getElementById(viewId);
      if (view) view.classList.add('active');

      // Zeitreise-View bei Aktivierung mit frischer Snapshot-Liste laden
      if (viewName === 'timetravel') refreshTimeTravel();
    });
  });
}

function updateLagDisplay(lag) {
  const lagEl = document.getElementById('projection-lag');
  if (!lagEl) return;
  lagEl.textContent = 'Lag: ' + lag;
  lagEl.className = lag > 100 ? 'lag-high' : lag > 10 ? 'lag-medium' : 'lag-ok';
}

// Nach Snapshot-Restore: World-State wurde ersetzt — Agents/Raeume aus REST
// neu laden (Projection ist nach resetWatermarks frisch) und abhaengige Views
// aktualisieren. (AC-4, #384)
async function reloadAfterRestore() {
  refreshTimeTravel();
}

function connectWebSocket() {
  const statusEl = document.getElementById('connection-status');
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  ws = new WebSocket(protocol + '//' + location.host + '/ws');

  ws.onopen = () => {
    statusEl.textContent = 'Verbunden';
    statusEl.classList.add('connected');
  };

  ws.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data);
      if (data.type === 'health_update') {
        updateLagDisplay(data.lag);
      } else if (data.type === 'snapshot_restored') {
        // Restore hat World-State ersetzt — Views aus REST neu laden (AC-4, #384)
        reloadAfterRestore();
      }
    } catch { /* ignore parse errors */ }
  };

  ws.onclose = () => {
    statusEl.textContent = 'Getrennt - Reconnect...';
    statusEl.classList.remove('connected');
    setTimeout(connectWebSocket, 3000);
  };

  ws.onerror = () => {
    statusEl.textContent = 'Fehler';
    statusEl.classList.remove('connected');
  };
}

async function init() {
  initNavigation();

  // Auth-Status frueh ermitteln, damit alle Views (Floorplan/Control/Zeitreise) den
  // korrekten Login-Zustand rendern (httpOnly-Cookie ist fuer JS unlesbar). (#402)
  await refreshAuthStatus();

  // Lade initiale Daten parallel
  try {
    // Control Panel async laden
    initControl();

    // Zeitreise-View async laden
    initTimeTravel();

    // Initiales Lag laden
    try {
      const healthRes = await fetch('/api/health');
      const health = await healthRes.json();
      updateLagDisplay(health.projection_lag);
    } catch { /* ignore */ }
  } catch (err) {
    console.error('Failed to load initial data:', err);
  }

  connectWebSocket();
}

document.addEventListener('DOMContentLoaded', init);
