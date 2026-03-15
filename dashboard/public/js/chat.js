// Chat-Log View: Zeigt Agent-Nachrichten (agent_action_received) pro Raum.
// Raum-Filter und chronologische Darstellung.

let currentRoom = null;
let cachedRooms = [];

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

  // Room buttons from cached /api/rooms data
  for (const room of cachedRooms) {
    const btn = document.createElement('button');
    btn.className = 'chat-filter-btn' + (currentRoom === room.id ? ' active' : '');
    btn.textContent = room.name || room.id;
    btn.addEventListener('click', () => {
      currentRoom = room.id;
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

  // Operator input section
  const inputSection = document.createElement('div');
  inputSection.className = 'chat-input-section';

  const input = document.createElement('textarea');
  input.id = 'chat-input';
  input.className = 'chat-input';
  input.placeholder = 'Nachricht an Agents eingeben...';
  input.rows = 2;
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendChatMessage();
    }
  });

  const sendBtn = document.createElement('button');
  sendBtn.className = 'chat-send-btn';
  sendBtn.textContent = 'Senden';
  sendBtn.addEventListener('click', sendChatMessage);

  inputSection.appendChild(input);
  inputSection.appendChild(sendBtn);
  wrapper.appendChild(inputSection);

  container.appendChild(wrapper);

  // Scroll to bottom (newest messages)
  list.scrollTop = list.scrollHeight;
}

function createChatMessage(msg) {
  const item = document.createElement('div');

  // Distinguish operator, gateway, and agent messages via CSS class
  const isOperator = msg.action_type === 'operator_message';
  const isGateway = msg.action_type === 'gateway_response';
  const isError = msg.action_type === 'error';
  let extraClass = '';
  if (isOperator) extraClass = ' chat-message-operator';
  else if (isGateway) extraClass = ' chat-message-gateway';
  else if (isError) extraClass = ' chat-message-error';
  item.className = 'chat-message' + extraClass;

  // Agent/Operator name
  const agent = document.createElement('span');
  agent.className = 'chat-agent';
  agent.textContent = msg.agent_name || msg.agent_id;
  item.appendChild(agent);

  // Action type badge (skip for operator/gateway — the CSS class handles that)
  if (msg.action_type && !isOperator && !isGateway && !isError) {
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
  if (msg.tick) metaText += ' — Tick ' + msg.tick;
  meta.textContent = metaText;
  item.appendChild(meta);

  return item;
}

async function sendChatMessage() {
  const input = document.getElementById('chat-input');
  if (!input) return;
  const message = input.value.trim();
  if (!message) return;

  // Clear input immediately for responsive UX
  input.value = '';
  input.disabled = true;

  try {
    const res = await fetch('/api/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ message, room: currentRoom }),
    });
    if (res.ok) {
      const data = await res.json();
      const list = document.getElementById('chat-list');
      if (list) {
        // Append operator message immediately
        list.appendChild(createChatMessage({
          agent_id: 'operator',
          agent_name: 'Operator',
          action_type: 'operator_message',
          content: data.message,
          target_room: data.room,
          tick: 0,
          timestamp_ms: Date.now(),
        }));

        // Append gateway response if available
        if (data.gateway_content) {
          list.appendChild(createChatMessage({
            agent_id: 'gateway',
            agent_name: 'Agent (Gateway)',
            action_type: 'gateway_response',
            content: data.gateway_content,
            target_room: data.room,
            tick: 0,
            timestamp_ms: Date.now(),
          }));
        } else if (data.gateway_response && data.gateway_response.error) {
          list.appendChild(createChatMessage({
            agent_id: 'gateway',
            agent_name: 'Gateway',
            action_type: 'error',
            content: data.gateway_response.error,
            target_room: data.room,
            tick: 0,
            timestamp_ms: Date.now(),
          }));
        }

        list.scrollTop = list.scrollHeight;
      }
      // Reload full chat after 1s to sync with DB (operator messages persist there)
      setTimeout(() => loadChat(), 1000);
    }
  } catch {
    // Send failed — restore message so user can retry
    if (input.value === '') input.value = message;
  } finally {
    input.disabled = false;
    input.focus();
  }
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
  try {
    const res = await fetch('/api/rooms');
    if (res.ok) {
      const rooms = await res.json();
      cachedRooms = rooms.sort((a, b) => (a.name || a.id).localeCompare(b.name || b.id));
    }
  } catch { /* rooms fetch failed, filter stays empty */ }
  await loadChat();
}
