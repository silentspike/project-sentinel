let currentRoom = null;

export function renderChat(messages) {
  const container = document.getElementById('view-chat');
  while (container.firstChild) container.removeChild(container.firstChild);

  const wrapper = document.createElement('div');
  wrapper.className = 'chat-container';

  // Room filter buttons
  const filter = document.createElement('div');
  filter.className = 'chat-filter';

  const rooms = ['kueche', 'buero-dev-1', 'buero-design-1', 'buero-ceo', 'meetingraum-01'];
  for (const roomId of rooms) {
    const btn = document.createElement('button');
    btn.textContent = roomId;
    btn.addEventListener('click', () => loadRoomChat(roomId, wrapper));
    if (roomId === 'kueche') btn.classList.add('active');
    filter.appendChild(btn);
  }
  wrapper.appendChild(filter);

  // Messages container
  const msgContainer = document.createElement('div');
  msgContainer.className = 'chat-messages';
  msgContainer.id = 'chat-messages';
  wrapper.appendChild(msgContainer);

  container.appendChild(wrapper);

  // Lade default
  loadRoomChat('kueche', wrapper);
}

async function loadRoomChat(roomId, wrapper) {
  currentRoom = roomId;

  // Active Button updaten
  const buttons = wrapper.querySelectorAll('.chat-filter button');
  buttons.forEach(b => {
    if (b.textContent === roomId) b.classList.add('active');
    else b.classList.remove('active');
  });

  try {
    const res = await fetch('/api/rooms/' + roomId + '/chat');
    const messages = await res.json();

    const msgContainer = document.getElementById('chat-messages');
    while (msgContainer.firstChild) msgContainer.removeChild(msgContainer.firstChild);

    for (const msg of messages) {
      const msgEl = document.createElement('div');
      msgEl.className = 'chat-message';

      const meta = document.createElement('div');
      meta.className = 'meta';

      const nameSpan = document.createElement('span');
      nameSpan.className = 'agent-name';
      nameSpan.textContent = msg.agent;
      meta.appendChild(nameSpan);

      const timeSpan = document.createElement('span');
      timeSpan.textContent = ' - ' + msg.timestamp;
      meta.appendChild(timeSpan);

      msgEl.appendChild(meta);

      const content = document.createElement('div');
      content.textContent = msg.message;
      msgEl.appendChild(content);

      msgContainer.appendChild(msgEl);
    }

    // Auto-scroll
    msgContainer.scrollTop = msgContainer.scrollHeight;
  } catch (err) {
    console.error('Failed to load chat:', err);
  }
}
