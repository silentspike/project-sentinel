export function renderAgents(agents) {
  const container = document.getElementById('view-agents');
  while (container.firstChild) container.removeChild(container.firstChild);

  const grid = document.createElement('div');
  grid.className = 'agents-grid';

  for (const agent of agents) {
    const card = createAgentCard(agent);
    grid.appendChild(card);
  }

  container.appendChild(grid);
}

function createAgentCard(agent) {
  const card = document.createElement('div');
  card.className = 'agent-card';
  card.setAttribute('data-agent-id', String(agent.id));

  const h3 = document.createElement('h3');
  h3.textContent = agent.name;
  card.appendChild(h3);

  const role = document.createElement('div');
  role.className = 'role';
  role.textContent = agent.role;
  card.appendChild(role);

  const status = document.createElement('div');
  status.className = 'status-badge status-' + agent.status;
  status.textContent = agent.status;
  card.appendChild(status);

  const room = document.createElement('div');
  room.className = 'room';
  if (agent.in_transit) {
    room.textContent = 'Unterwegs' + (agent.transit_target ? ' \u2192 ' + agent.transit_target : '');
    room.classList.add('transit');
  } else {
    room.textContent = agent.room_name || agent.current_room || '\u2014';
  }
  card.appendChild(room);

  // Detail-Daten async laden
  loadAgentDetail(agent.id, card);

  return card;
}

async function loadAgentDetail(agentId, card) {
  try {
    const res = await fetch('/api/agents/' + agentId + '/state');
    if (!res.ok) return;
    const data = await res.json();

    if (data.last_action) {
      const action = document.createElement('div');
      action.className = 'last-action';
      action.textContent = data.last_action;
      card.appendChild(action);
    }

    const meta = document.createElement('div');
    meta.className = 'agent-meta';
    meta.textContent = 'Schicht ' + data.shift_set;
    card.appendChild(meta);
  } catch { /* silently ignore */ }
}

export function updateAgents(agents) {
  renderAgents(agents);
}
