// Router und WebSocket Manager
import { renderAgents, updateAgentBio } from './agents.js';
import { renderFloorplan } from './floorplan.js';
import { renderChat } from './chat.js';
import { renderMetrics } from './metrics.js';

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
      if (data.type === 'bio_update') {
        updateAgentBio(data.agent, data.data);
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
    const [agentsRes, roomsRes, metricsRes] = await Promise.all([
      fetch('/api/agents'),
      fetch('/api/rooms'),
      fetch('/api/metrics')
    ]);

    const agents = await agentsRes.json();
    const rooms = await roomsRes.json();
    const metrics = await metricsRes.json();

    renderAgents(agents);
    renderFloorplan(rooms);
    renderMetrics(metrics);
    renderChat([]); // Chat wird on-demand geladen
  } catch (err) {
    console.error('Failed to load initial data:', err);
  }

  connectWebSocket();
}

document.addEventListener('DOMContentLoaded', init);
