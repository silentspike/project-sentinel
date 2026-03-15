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

  const STATUS_LABELS = { active: 'Aktiv', suspended: 'Pausiert', errored: 'Fehler', despawned: 'Despawned' };
  const status = document.createElement('div');
  status.className = 'status-badge status-' + agent.status;
  status.textContent = STATUS_LABELS[agent.status] || agent.status;
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

  // Stall indicator (from eBPF monitoring)
  if (agent.stalled) {
    card.classList.add('agent-stalled');
    var stall = document.createElement('div');
    stall.className = 'stall-indicator';
    stall.textContent = 'Stalled';
    card.appendChild(stall);
  }

  // Bio-State Bars (async geladen)
  loadAgentDetail(agent.id, card);

  return card;
}

function createBioBar(label, value) {
  const bar = document.createElement('div');
  bar.className = 'bio-bar';

  const labelEl = document.createElement('span');
  labelEl.className = 'bio-bar-label';
  labelEl.textContent = label;
  bar.appendChild(labelEl);

  const track = document.createElement('div');
  track.className = 'bio-bar-track';

  const fill = document.createElement('div');
  fill.className = 'bio-bar-fill';

  if (value != null && !isNaN(value)) {
    // Bio values are already 0-100 from the API
    const pct = Math.min(100, Math.max(0, Math.round(value)));
    fill.style.width = pct + '%';
    if (pct > 70) {
      fill.classList.add('bio-bar-high');
    } else if (pct > 40) {
      fill.classList.add('bio-bar-mid');
    } else {
      fill.classList.add('bio-bar-low');
    }
  } else {
    fill.style.width = '0%';
    fill.classList.add('bio-bar-empty');
  }

  track.appendChild(fill);
  bar.appendChild(track);

  const valueEl = document.createElement('span');
  valueEl.className = 'bio-bar-value';
  valueEl.textContent = value != null ? Math.min(100, Math.max(0, Math.round(value))) + '%' : '--';
  bar.appendChild(valueEl);

  return bar;
}

async function loadAgentDetail(agentId, card) {
  try {
    const res = await fetch('/api/agents/' + agentId + '/state');
    if (!res.ok) return;
    const data = await res.json();

    const action = document.createElement('div');
    action.className = 'last-action';
    if (data.last_action) {
      action.textContent = data.last_action;
      if (data.last_action_tick != null) {
        const tickSpan = document.createElement('span');
        tickSpan.className = 'last-action-tick';
        tickSpan.textContent = ' T' + data.last_action_tick;
        action.appendChild(tickSpan);
      }
    } else {
      action.textContent = 'Keine Aktion';
      action.classList.add('last-action-empty');
    }
    card.appendChild(action);

    const meta = document.createElement('div');
    meta.className = 'agent-meta';
    const agentIdStr = 'AGENT-' + String(agentId).padStart(2, '0');
    meta.textContent = agentIdStr + ' | Schicht ' + data.shift_set;
    card.appendChild(meta);

    // Mood
    const moodEl = document.createElement('div');
    moodEl.className = 'agent-mood';
    moodEl.textContent = 'Stimmung: ' + (data.mood || '\u2014');
    card.appendChild(moodEl);

    // Bio-State Bars (nur anzeigen wenn mindestens ein Wert vorhanden)
    const hasBio = data.hunger != null || data.energy != null || data.stress != null;
    if (hasBio) {
      const bioSection = document.createElement('div');
      bioSection.className = 'bio-section';
      bioSection.appendChild(createBioBar('Hunger', data.hunger));
      bioSection.appendChild(createBioBar('Energie', data.energy));
      bioSection.appendChild(createBioBar('Stress', data.stress));
      bioSection.appendChild(createBioBar('Koffein', data.caffeine_mg));
      bioSection.appendChild(createBioBar('Blase', data.bladder));
      bioSection.appendChild(createBioBar('Sozial', data.social_need));
      card.appendChild(bioSection);
    }
  } catch { /* silently ignore */ }
}

export function updateAgents(agents) {
  const container = document.getElementById('view-agents');
  const grid = container.querySelector('.agents-grid');

  // Fallback: full render if grid doesn't exist yet
  if (!grid) {
    renderAgents(agents);
    return;
  }

  const existingCards = new Map();
  for (const card of grid.querySelectorAll('.agent-card')) {
    existingCards.set(card.getAttribute('data-agent-id'), card);
  }

  for (const agent of agents) {
    const card = existingCards.get(String(agent.id));
    if (!card) {
      // New agent — append card
      const newCard = createAgentCard(agent);
      grid.appendChild(newCard);
      continue;
    }

    // Differential update: only touch changed fields
    const STATUS_LABELS_U = { active: 'Aktiv', suspended: 'Pausiert', errored: 'Fehler', despawned: 'Despawned' };
    const statusBadge = card.querySelector('.status-badge');
    const statusLabel = STATUS_LABELS_U[agent.status] || agent.status;
    if (statusBadge && statusBadge.textContent !== statusLabel) {
      statusBadge.textContent = statusLabel;
      statusBadge.className = 'status-badge status-' + agent.status;
    }

    const roomEl = card.querySelector('.room');
    if (roomEl) {
      let newText;
      let isTransit = false;
      if (agent.in_transit) {
        newText = 'Unterwegs' + (agent.transit_target ? ' \u2192 ' + agent.transit_target : '');
        isTransit = true;
      } else {
        newText = agent.room_name || agent.current_room || '\u2014';
      }
      if (roomEl.textContent !== newText) {
        roomEl.textContent = newText;
        roomEl.classList.toggle('transit', isTransit);
      }
    }

    // Differential update: stall indicator
    const wasStalled = card.classList.contains('agent-stalled');
    if (agent.stalled && !wasStalled) {
      card.classList.add('agent-stalled');
      var stallEl = document.createElement('div');
      stallEl.className = 'stall-indicator';
      stallEl.textContent = 'Stalled';
      card.appendChild(stallEl);
    } else if (!agent.stalled && wasStalled) {
      card.classList.remove('agent-stalled');
      var stallInd = card.querySelector('.stall-indicator');
      if (stallInd) stallInd.remove();
    }

    // Differential update: last_action
    const actionEl = card.querySelector('.last-action');
    if (actionEl) {
      const newAction = agent.last_action || '';
      const oldAction = actionEl.getAttribute('data-action') || '';
      if (newAction !== oldAction) {
        actionEl.textContent = '';
        actionEl.setAttribute('data-action', newAction);
        actionEl.classList.remove('last-action-empty');
        if (newAction) {
          actionEl.textContent = newAction;
          if (agent.last_action_tick != null) {
            const tickSpan = document.createElement('span');
            tickSpan.className = 'last-action-tick';
            tickSpan.textContent = ' T' + agent.last_action_tick;
            actionEl.appendChild(tickSpan);
          }
        } else {
          actionEl.textContent = 'Keine Aktion';
          actionEl.classList.add('last-action-empty');
        }
      }
    }

    existingCards.delete(String(agent.id));
  }
}
