// Router und WebSocket Manager
import { renderAgents, updateAgents } from './agents.js';
import { renderFloorplan } from './floorplan.js';
import { renderActivity, updateActivity } from './activity.js';
import { renderMetrics } from './metrics.js';
import { renderCockpit, updateCockpit } from './cockpit.js';
import { renderChaos, updateChaos } from './chaos.js';
import { renderChat, initChat } from './chat.js';
import { initControl } from './control.js';

let ws = null;

function initNavigation() {
  const nav = document.getElementById('main-nav');
  const buttons = nav.querySelectorAll('.nav-btn');

  buttons.forEach(btn => {
    btn.addEventListener('click', () => {
      buttons.forEach(b => b.classList.remove('active'));
      btn.classList.add('active');

      document.querySelectorAll('.view').forEach(v => v.classList.remove('active'));
      const viewId = 'view-' + btn.getAttribute('data-view');
      const view = document.getElementById(viewId);
      if (view) view.classList.add('active');
    });
  });
}

function updateLagDisplay(lag) {
  const lagEl = document.getElementById('projection-lag');
  if (!lagEl) return;
  lagEl.textContent = 'Lag: ' + lag;
  lagEl.className = lag > 100 ? 'lag-high' : lag > 10 ? 'lag-medium' : 'lag-ok';
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
      if (data.type === 'agent_update') {
        updateAgents(data.agents);
        updateActivity();
      } else if (data.type === 'room_update') {
        renderFloorplan(data.rooms);
      } else if (data.type === 'health_update') {
        updateLagDisplay(data.lag);
      } else if (data.type === 'cockpit_update') {
        updateCockpit();
      } else if (data.type === 'chaos_update') {
        updateChaos();
      } else if (data.type === 'activity_update') {
        updateActivity();
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

  // Lade initiale Daten parallel
  try {
    const [agentsRes, roomsRes, metricsRes, cockpitRes, chaosRes] = await Promise.all([
      fetch('/api/agents'),
      fetch('/api/rooms'),
      fetch('/api/metrics'),
      fetch('/api/cockpit'),
      fetch('/api/chaos?limit=100'),
    ]);

    const agents = await agentsRes.json();
    const rooms = await roomsRes.json();
    const metrics = await metricsRes.json();
    const cockpit = await cockpitRes.json();
    const chaos = await chaosRes.json();

    renderAgents(agents);
    renderFloorplan(rooms);
    renderMetrics(metrics);
    renderActivity();
    renderCockpit(cockpit);
    renderChaos(chaos);

    // Chat async laden (eigener Fetch)
    initChat();

    // Control Panel async laden
    initControl();

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
