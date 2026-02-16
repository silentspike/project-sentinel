export function renderActivity(agents) {
  const container = document.getElementById('view-activity');
  while (container.firstChild) container.removeChild(container.firstChild);

  const wrapper = document.createElement('div');
  wrapper.className = 'activity-container';

  const h2 = document.createElement('h2');
  h2.textContent = 'Letzte Aktivitäten';
  wrapper.appendChild(h2);

  const list = document.createElement('div');
  list.className = 'activity-list';
  list.id = 'activity-list';

  // Aus Agent-Daten Aktivitäten ableiten
  const activities = buildActivities(agents);
  for (const act of activities) {
    list.appendChild(createActivityItem(act));
  }

  if (activities.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'activity-empty';
    empty.textContent = 'Keine Aktivitäten vorhanden';
    list.appendChild(empty);
  }

  wrapper.appendChild(list);
  container.appendChild(wrapper);
}

function buildActivities(agents) {
  const activities = [];

  for (const agent of agents) {
    if (agent.in_transit) {
      activities.push({
        type: 'transit',
        agent: agent.name,
        detail: 'Unterwegs' + (agent.transit_target ? ' nach ' + agent.transit_target : ''),
        tick: agent.last_action_tick || 0,
      });
    }
    if (agent.last_action) {
      activities.push({
        type: 'action',
        agent: agent.name,
        detail: agent.last_action,
        tick: agent.last_action_tick || 0,
      });
    }
  }

  // Sortiere nach Tick (neueste zuerst)
  activities.sort((a, b) => b.tick - a.tick);
  return activities.slice(0, 50);
}

function createActivityItem(act) {
  const item = document.createElement('div');
  item.className = 'activity-item activity-' + act.type;

  const agent = document.createElement('span');
  agent.className = 'activity-agent';
  agent.textContent = act.agent;
  item.appendChild(agent);

  const detail = document.createElement('span');
  detail.className = 'activity-detail';
  detail.textContent = ' ' + act.detail;
  item.appendChild(detail);

  return item;
}

export function updateActivity(agents) {
  renderActivity(agents);
}
