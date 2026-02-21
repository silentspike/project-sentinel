// Chat-Log View: Zeigt Agent-Nachrichten (agent_action_received) pro Raum.
// Raum-Filter und chronologische Darstellung.

let currentRoom = null;

export function renderChat(messages) {
  const container = document.getElementById('view-chat');
  while (container.firstChild) container.removeChild(container.firstChild);

  const wrapper = document.createElement('div');
  wrapper.className = 'chat-container';

  // Room filter bar
  const filterBar = document.createElement('div');
  filterBar.className = 'chat-filter-bar';

  const filterLabel = document.createElement('span');
  filterLabel.className = 'chat-filter-label';
  filterLabel.textContent = 'Raum: ';
  filterBar.appendChild(filterLabel);

  const allBtn = document.createElement('button');
  allBtn.className = 'chat-filter-btn' + (currentRoom === null ? ' active' : '');
  allBtn.textContent = 'Alle';
  allBtn.addEventListener('click', () => {
    currentRoom = null;
    loadChat();
  });
  filterBar.appendChild(allBtn);

  // Extract unique rooms from messages
  const rooms = [...new Set(messages.filter(m => m.target_room).map(m => m.target_room))];
  rooms.sort();
  for (const room of rooms) {
    const btn = document.createElement('button');
    btn.className = 'chat-filter-btn' + (currentRoom === room ? ' active' : '');
    btn.textContent = room;
    btn.addEventListener('click', () => {
      currentRoom = room;
      loadChat();
    });
    filterBar.appendChild(btn);
  }

  wrapper.appendChild(filterBar);

  // Message list
  const list = document.createElement('div');
  list.className = 'chat-list';
  list.id = 'chat-list';

  // Reverse to show oldest first (chronological)
  const sorted = [...messages].reverse();
  for (const msg of sorted) {
    list.appendChild(createChatMessage(msg));
  }

  if (messages.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'chat-empty';
    empty.textContent = 'Keine Nachrichten vorhanden';
    list.appendChild(empty);
  }

  wrapper.appendChild(list);
  container.appendChild(wrapper);

  // Scroll to bottom (newest messages)
  list.scrollTop = list.scrollHeight;
}

function createChatMessage(msg) {
  const item = document.createElement('div');
  item.className = 'chat-message';

  // Agent name
  const agent = document.createElement('span');
  agent.className = 'chat-agent';
  agent.textContent = msg.agent_name || msg.agent_id;
  item.appendChild(agent);

  // Action type badge
  if (msg.action_type) {
    const badge = document.createElement('span');
    badge.className = 'chat-action-badge';
    badge.textContent = msg.action_type;
    item.appendChild(badge);
  }

  // Content
  if (msg.content) {
    const content = document.createElement('div');
    content.className = 'chat-content';
    content.textContent = msg.content;
    item.appendChild(content);
  }

  // Room + Timestamp
  const meta = document.createElement('div');
  meta.className = 'chat-meta';
  const date = new Date(msg.timestamp_ms);
  let metaText = date.toLocaleString('de-DE');
  if (msg.target_room) metaText += ' — ' + msg.target_room;
  metaText += ' — Tick ' + msg.tick;
  meta.textContent = metaText;
  item.appendChild(meta);

  return item;
}

async function loadChat() {
  try {
    const url = currentRoom ? '/api/chat/' + currentRoom : '/api/chat';
    const res = await fetch(url);
    const messages = await res.json();
    renderChat(messages);
  } catch {
    // Fetch failed
  }
}

export async function updateChat() {
  await loadChat();
}

export async function initChat() {
  await loadChat();
}
