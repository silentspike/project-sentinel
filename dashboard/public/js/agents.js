export function renderAgents(agents) {
  const container = document.getElementById('view-agents');
  // Container leeren
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
  card.setAttribute('data-agent', agent.name);

  const h3 = document.createElement('h3');
  h3.textContent = agent.name;
  card.appendChild(h3);

  const role = document.createElement('div');
  role.className = 'role';
  role.textContent = agent.role;
  card.appendChild(role);

  const room = document.createElement('div');
  room.className = 'room';
  room.textContent = agent.room;
  card.appendChild(room);

  const emotion = document.createElement('div');
  emotion.className = 'emotion';
  emotion.textContent = agent.mood ? agent.mood.emotion : 'neutral';
  card.appendChild(emotion);

  // Lade Bio-Daten async fuer Detail-View
  loadAgentBio(agent.name, card);

  return card;
}

async function loadAgentBio(name, card) {
  try {
    const slug = name.toLowerCase().replace(/\s+/g, '-');
    const res = await fetch('/api/agents/' + slug + '/state');
    if (!res.ok) return;
    const data = await res.json();

    if (data.bio) {
      const bars = createBioBars(data.bio);
      card.appendChild(bars);
    }
  } catch { /* silently ignore */ }
}

function createBioBars(bio) {
  const container = document.createElement('div');
  container.className = 'bio-bars';

  const fields = [
    { label: 'Hunger', value: bio.hunger },
    { label: 'Energie', value: bio.energy },
    { label: 'Blase', value: bio.bladder },
    { label: 'Stress', value: bio.stress },
    { label: 'Sozial', value: bio.social_need },
  ];

  for (const field of fields) {
    const row = document.createElement('div');
    row.className = 'bio-bar';

    const label = document.createElement('label');
    label.textContent = field.label;
    row.appendChild(label);

    const bar = document.createElement('div');
    bar.className = 'bar';

    const fill = document.createElement('div');
    fill.className = 'bar-fill ' + getBarClass(field.value);
    fill.style.width = Math.min(100, Math.max(0, field.value)) + '%';
    bar.appendChild(fill);

    row.appendChild(bar);
    container.appendChild(row);
  }

  return container;
}

function getBarClass(value) {
  if (value < 40) return 'good';
  if (value < 70) return 'medium';
  return 'bad';
}

export function updateAgentBio(agentName, bioData) {
  const card = document.querySelector('.agent-card[data-agent="' + agentName + '"]');
  if (!card) return;

  const bars = card.querySelector('.bio-bars');
  if (bars) {
    card.removeChild(bars);
  }
  card.appendChild(createBioBars(bioData));
}
