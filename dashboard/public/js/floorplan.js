const CHAOS_OPTIONS = [
  ['AirConBroken', 'Klimaanlage defekt'],
  ['PrinterBroken', 'Drucker defekt'],
  ['PhoneRing', 'Telefon klingelt'],
  ['PackageDelivery', 'Paketlieferung'],
  ['SBahnDelay', 'S-Bahn Verspaetung'],
  ['FireAlarmDrill', 'Feueralarm-Uebung'],
  ['CakeInKitchen', 'Kuchen in der Kueche'],
  ['InternetOutage', 'Internetausfall'],
];

const STIMULUS_OPTIONS = [
  ['temperature', 'Temperatur', 4, '°C'],
  ['noise', 'Laerm', 24, 'dB'],
  ['co2', 'CO2', 900, 'ppm'],
];

let latestRooms = [];
let activeRoomId = null;
let activeRoomDetail = null;
let detailLoading = false;
let detailError = '';
let detailRequestId = 0;
let chaosTriggerState = {
  pending: false,
  message: '',
  error: false,
  chaosType: 'AirConBroken',
  durationTicks: '',
};
let stimulusTriggerState = {
  pending: false,
  message: '',
  error: false,
  stimulusType: 'temperature',
  delta: '4',
  durationTicks: '',
};

function getFloorplanContainer() {
  return document.getElementById('view-floorplan');
}

function getApiKey() {
  return sessionStorage.getItem('sentinel_api_key') || '';
}

function authHeaders() {
  const key = getApiKey();
  return key ? { Authorization: 'Bearer ' + key } : {};
}

function createEl(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text != null) el.textContent = text;
  return el;
}

function readSelectedValue(select) {
  if (typeof select.value === 'string' && select.value) return select.value;
  for (const option of Array.from(select.options || [])) {
    if (option.selected) return option.value;
  }
  return '';
}

function formatMetric(value, suffix, digits = 0) {
  if (value == null) return 'n/a';
  const rounded = digits > 0 ? Number(value).toFixed(digits) : String(Math.round(value));
  return rounded + ' ' + suffix;
}

function describeChaos(chaos) {
  if (!chaos || typeof chaos !== 'object') return 'Kein aktives Chaos';
  return chaos.description || chaos.event_type || chaos.type || 'Chaos aktiv';
}

function syncRoomSnapshot(detail) {
  if (!detail || !detail.id) return;
  latestRooms = latestRooms.map((room) =>
    room.id !== detail.id
      ? room
      : {
          ...room,
          occupant_count: detail.occupant_count,
          transit_count: detail.transit_count,
          active_chaos: detail.active_chaos,
          active_smells: detail.active_smells,
          temperature: detail.temperature,
          co2_ppm: detail.co2_ppm,
          noise_db: detail.noise_db,
          last_event_tick: detail.last_event_tick,
          occupants: Array.isArray(detail.occupants) ? detail.occupants : room.occupants,
        },
  );
}

async function loadRoomDetail(roomId, options = {}) {
  const { silent = false } = options;
  const requestId = ++detailRequestId;
  activeRoomId = roomId;
  if (!silent) {
    detailLoading = true;
    detailError = '';
    activeRoomDetail = null;
    renderFloorplan(latestRooms);
  }

  try {
    const res = await fetch('/api/rooms/' + encodeURIComponent(roomId) + '/detail');
    if (!res.ok) {
      let message = 'Detaildaten konnten nicht geladen werden';
      try {
        const payload = await res.json();
        message = payload.error || message;
      } catch {
        // ignore parse errors
      }
      throw new Error(message);
    }
    const detail = await res.json();
    if (requestId !== detailRequestId || activeRoomId !== roomId) return;
    activeRoomDetail = detail;
    syncRoomSnapshot(detail);
    detailError = '';
  } catch (err) {
    if (requestId !== detailRequestId || activeRoomId !== roomId) return;
    activeRoomDetail = null;
    detailError = err instanceof Error ? err.message : String(err);
  } finally {
    if (requestId !== detailRequestId || activeRoomId !== roomId) return;
    detailLoading = false;
    renderFloorplan(latestRooms);
  }
}

function closeRoomDetail() {
  activeRoomId = null;
  activeRoomDetail = null;
  detailLoading = false;
  detailError = '';
  chaosTriggerState = {
    pending: false,
    message: '',
    error: false,
    chaosType: 'AirConBroken',
    durationTicks: '',
  };
  stimulusTriggerState = {
    pending: false,
    message: '',
    error: false,
    stimulusType: 'temperature',
    delta: '4',
    durationTicks: '',
  };
  renderFloorplan(latestRooms);
}

function onCardActivate(roomId) {
  chaosTriggerState.message = '';
  chaosTriggerState.error = false;
  stimulusTriggerState.message = '';
  stimulusTriggerState.error = false;
  void loadRoomDetail(roomId);
}

async function submitChaosTrigger(event) {
  event.preventDefault();
  if (!activeRoomId || chaosTriggerState.pending) return;

  chaosTriggerState.pending = true;
  chaosTriggerState.message = '';
  chaosTriggerState.error = false;
  renderFloorplan(latestRooms);

  const payload = {
    room_id: activeRoomId,
    chaos_type: chaosTriggerState.chaosType,
  };
  const duration = parseInt(chaosTriggerState.durationTicks, 10);
  if (!Number.isNaN(duration) && duration > 0) {
    payload.duration_ticks = duration;
  }

  try {
    const res = await fetch('/api/control/chaos', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...authHeaders(),
      },
      body: JSON.stringify(payload),
    });

    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      throw new Error(data.error || data.detail || res.statusText);
    }

    chaosTriggerState.message = 'Chaos-Trigger angenommen: ' + (data.event_id || 'ok');
    chaosTriggerState.error = false;
    chaosTriggerState.durationTicks = '';
    await loadRoomDetail(activeRoomId, { silent: true });
  } catch (err) {
    chaosTriggerState.message = err instanceof Error ? err.message : String(err);
    chaosTriggerState.error = true;
    renderFloorplan(latestRooms);
  } finally {
    chaosTriggerState.pending = false;
    renderFloorplan(latestRooms);
  }
}

function setStimulusType(type) {
  stimulusTriggerState.stimulusType = type;
  const selected = STIMULUS_OPTIONS.find(([value]) => value === type);
  if (selected) {
    stimulusTriggerState.delta = String(selected[2]);
  }
}

async function submitStimulusTrigger(event) {
  event.preventDefault();
  if (!activeRoomId || stimulusTriggerState.pending) return;

  stimulusTriggerState.pending = true;
  stimulusTriggerState.message = '';
  stimulusTriggerState.error = false;
  renderFloorplan(latestRooms);

  const delta = parseFloat(stimulusTriggerState.delta);
  if (Number.isNaN(delta) || delta === 0) {
    stimulusTriggerState.pending = false;
    stimulusTriggerState.error = true;
    stimulusTriggerState.message = 'Bitte ein gueltiges Delta ungleich 0 eingeben';
    renderFloorplan(latestRooms);
    return;
  }

  const payload = {
    room_id: activeRoomId,
    stimulus_type: stimulusTriggerState.stimulusType,
    delta,
  };
  const duration = parseInt(stimulusTriggerState.durationTicks, 10);
  if (!Number.isNaN(duration) && duration > 0) {
    payload.duration_ticks = duration;
  }

  try {
    const res = await fetch('/api/control/stimulus', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...authHeaders(),
      },
      body: JSON.stringify(payload),
    });

    const data = await res.json().catch(() => ({}));
    if (!res.ok) {
      throw new Error(data.error || data.detail || res.statusText);
    }

    stimulusTriggerState.message = 'Raumreiz angenommen: ' + (data.event_id || 'ok');
    stimulusTriggerState.error = false;
    stimulusTriggerState.durationTicks = '';
    await loadRoomDetail(activeRoomId, { silent: true });
  } catch (err) {
    stimulusTriggerState.message = err instanceof Error ? err.message : String(err);
    stimulusTriggerState.error = true;
  } finally {
    stimulusTriggerState.pending = false;
    renderFloorplan(latestRooms);
  }
}

function getPerceptionHints(detail) {
  const hints = [];
  if (detail.temperature != null) {
    if (detail.temperature > 24) hints.push('Prompt-Hinweis: Es ist warm');
    else if (detail.temperature < 19) hints.push('Prompt-Hinweis: Es ist kuehl');
  }
  if (detail.co2_ppm != null && detail.co2_ppm > 1000) {
    hints.push('Prompt-Hinweis: Die Luft ist stickig');
  }
  if (detail.noise_db != null) {
    if (detail.noise_db > 65) hints.push('Prompt-Hinweis: Es ist laut');
    else if (detail.noise_db > 50) hints.push('Prompt-Hinweis: Lebhafte Unterhaltungen');
  }
  return hints;
}

function createRoomCard(room) {
  const card = document.createElement('button');
  card.type = 'button';
  card.className = 'room-card room-card-button';
  if (activeRoomId === room.id) card.classList.add('active');
  card.setAttribute('data-room-id', room.id);
  card.setAttribute('aria-pressed', activeRoomId === room.id ? 'true' : 'false');
  card.addEventListener('click', () => onCardActivate(room.id));

  const h4 = createEl('h4', '', room.name);
  card.appendChild(h4);

  const type = createEl('div', 'room-type', (room.room_type || '').toUpperCase());
  card.appendChild(type);

  const occ = createEl(
    'div',
    'room-occupancy' + (room.occupant_count > 0 ? ' occupied' : ''),
    room.occupant_count + '/' + room.capacity + ' Personen',
  );
  card.appendChild(occ);

  if (room.occupants && room.occupants.length > 0) {
    const agentList = createEl('div', 'room-agents');
    for (const name of room.occupants) {
      agentList.appendChild(createEl('span', 'room-agent-tag', name));
    }
    card.appendChild(agentList);
  }

  if (room.temperature != null || room.noise_db != null || room.co2_ppm != null) {
    const parts = [];
    if (room.temperature != null) parts.push(formatMetric(room.temperature, '°C', 1));
    if (room.co2_ppm != null) parts.push(formatMetric(room.co2_ppm, 'ppm'));
    if (room.noise_db != null) parts.push(formatMetric(room.noise_db, 'dB'));
    card.appendChild(createEl('div', 'room-physics', parts.join(' | ')));
  }

  if (room.transit_count > 0) {
    card.appendChild(
      createEl('div', 'transit-indicator', room.transit_count + ' unterwegs'),
    );
  }

  if (room.active_chaos) {
    const chaosLabel =
      room.active_chaos.event_type || room.active_chaos.type || 'Chaos aktiv';
    card.appendChild(createEl('div', 'chaos-badge', chaosLabel));
  }

  const footer = createEl('div', 'room-card-footer');
  footer.appendChild(createEl('span', 'room-card-link', 'Details'));
  card.appendChild(footer);

  return card;
}

function renderHistoryList(items, emptyText, renderItem) {
  if (!items || items.length === 0) {
    return createEl('div', 'room-detail-empty', emptyText);
  }
  const list = createEl('div', 'room-history-list');
  for (const item of items) {
    list.appendChild(renderItem(item));
  }
  return list;
}

function renderDetailSnapshot(detail) {
  const section = createEl('section', 'room-detail-section');
  section.appendChild(createEl('h3', 'room-detail-heading', 'Snapshot'));

  const grid = createEl('div', 'room-detail-metrics');
  const metrics = [
    ['Temperatur', formatMetric(detail.temperature, '°C', 1)],
    ['CO2', formatMetric(detail.co2_ppm, 'ppm')],
    ['Laerm', formatMetric(detail.noise_db, 'dB')],
    ['Belegung', String(detail.occupant_count)],
    ['Transit', String(detail.transit_count)],
    ['Letzter Tick', detail.last_event_tick == null ? 'n/a' : 't' + detail.last_event_tick],
  ];
  for (const [label, value] of metrics) {
    const item = createEl('div', 'room-detail-metric');
    item.appendChild(createEl('span', 'room-detail-metric-label', label));
    item.appendChild(createEl('strong', 'room-detail-metric-value', value));
    grid.appendChild(item);
  }
  section.appendChild(grid);

  const occupants = createEl('div', 'room-detail-subsection');
  occupants.appendChild(createEl('h4', 'room-detail-subheading', 'Anwesende Agents'));
  if (detail.occupants && detail.occupants.length > 0) {
    const tags = createEl('div', 'room-agents');
    for (const name of detail.occupants) {
      tags.appendChild(createEl('span', 'room-agent-tag', name));
    }
    occupants.appendChild(tags);
  } else {
    occupants.appendChild(createEl('div', 'room-detail-empty', 'Keine Agents im Raum'));
  }
  section.appendChild(occupants);

  const chaos = createEl('div', 'room-detail-subsection');
  chaos.appendChild(createEl('h4', 'room-detail-subheading', 'Aktives Chaos'));
  chaos.appendChild(
    createEl(
      'div',
      detail.active_chaos ? 'room-detail-chaos active' : 'room-detail-chaos',
      describeChaos(detail.active_chaos),
    ),
  );
  section.appendChild(chaos);

  const perception = createEl('div', 'room-detail-subsection');
  perception.appendChild(createEl('h4', 'room-detail-subheading', 'Prompt-Hinweise'));
  const hints = getPerceptionHints(detail);
  if (hints.length > 0) {
    const list = createEl('div', 'room-history-list');
    for (const hint of hints) {
      const item = createEl('div', 'room-history-item');
      item.appendChild(createEl('div', 'room-history-meta', hint));
      list.appendChild(item);
    }
    perception.appendChild(list);
  } else {
    perception.appendChild(
      createEl('div', 'room-detail-empty', 'Aktuell keine auffaelligen Umweltreize im Prompt'),
    );
  }
  section.appendChild(perception);

  return section;
}

function renderPhysicsSection(detail) {
  const section = createEl('section', 'room-detail-section');
  section.appendChild(createEl('h3', 'room-detail-heading', 'Physics-Verlauf'));
  section.appendChild(
    renderHistoryList(detail.physics_history, 'Noch keine Physics-Historie vorhanden', (entry) => {
      const item = createEl('div', 'room-history-item');
      item.appendChild(createEl('strong', 'room-history-title', 't' + entry.tick));
      item.appendChild(
        createEl(
          'div',
          'room-history-meta',
          [
            formatMetric(entry.temperature, '°C', 1),
            formatMetric(entry.co2_ppm, 'ppm'),
            formatMetric(entry.noise_db, 'dB'),
            entry.occupant_count + ' Pers.',
          ].join(' | '),
        ),
      );
      return item;
    }),
  );
  return section;
}

function renderChaosSection(detail) {
  const section = createEl('section', 'room-detail-section');
  section.appendChild(createEl('h3', 'room-detail-heading', 'Chaos-Historie'));
  section.appendChild(
    renderHistoryList(detail.chaos_history, 'Noch keine Chaos-Events vorhanden', (entry) => {
      const item = createEl('div', 'room-history-item chaos');
      item.appendChild(
        createEl(
          'strong',
          'room-history-title',
          entry.chaos_type + (entry.room_id ? ' in ' + entry.room_id : ''),
        ),
      );
      item.appendChild(createEl('div', 'room-history-meta', entry.description || 'ohne Beschreibung'));
      item.appendChild(createEl('div', 'room-history-meta', 'Tick ' + entry.tick));
      return item;
    }),
  );
  return section;
}

function renderReactionSection(detail) {
  const section = createEl('section', 'room-detail-section');
  section.appendChild(createEl('h3', 'room-detail-heading', 'Reaktionen im Raum'));
  section.appendChild(
    renderHistoryList(detail.recent_reactions, 'Noch keine Reaktionen im Zeitfenster', (entry) => {
      const item = createEl('div', 'room-history-item reaction');
      item.appendChild(
        createEl(
          'strong',
          'room-history-title',
          entry.agent_name + ' - ' + (entry.action_type || 'Aktion'),
        ),
      );
      item.appendChild(
        createEl(
          'div',
          'room-history-meta',
          (entry.content || 'ohne Details') + ' | Tick ' + entry.tick,
        ),
      );
      if (entry.stimulus_type) {
        const context =
          entry.stimulus_tick == null
            ? 'Kontext: nach Raumreiz ' + entry.stimulus_type
            : 'Kontext: nach Raumreiz ' + entry.stimulus_type + ' seit t' + entry.stimulus_tick;
        item.appendChild(createEl('div', 'room-history-meta', context));
      } else if (entry.chaos_type) {
        const context =
          entry.chaos_tick == null
            ? 'Kontext: ' + entry.chaos_type
            : 'Kontext: nach ' + entry.chaos_type + ' seit t' + entry.chaos_tick;
        item.appendChild(createEl('div', 'room-history-meta', context));
      }
      return item;
    }),
  );
  return section;
}

function renderStimulusSection(detail) {
  const section = createEl('section', 'room-detail-section');
  section.appendChild(createEl('h3', 'room-detail-heading', 'Raumreiz testen'));

  if (!getApiKey()) {
    const authBox = createEl('div', 'room-detail-auth');
    authBox.appendChild(
      createEl('div', 'room-detail-note', 'API-Key eingeben um Raumreize auszuloesen:'),
    );
    const keyInput = document.createElement('input');
    keyInput.type = 'password';
    keyInput.className = 'room-trigger-input';
    keyInput.placeholder = 'Operator API-Key';
    keyInput.addEventListener('change', () => {
      if (keyInput.value.trim()) {
        sessionStorage.setItem('sentinel_api_key', keyInput.value.trim());
        renderFloorplan(latestRooms);
      }
    });
    authBox.appendChild(keyInput);
    section.appendChild(authBox);
  }

  section.appendChild(
    renderHistoryList(detail.stimulus_history, 'Noch keine Raumreize vorhanden', (entry) => {
      const item = createEl('div', 'room-history-item');
      item.appendChild(
        createEl(
          'strong',
          'room-history-title',
          entry.stimulus_type + ' ' + (entry.delta > 0 ? '+' : '') + entry.delta,
        ),
      );
      item.appendChild(createEl('div', 'room-history-meta', entry.description || 'ohne Beschreibung'));
      item.appendChild(createEl('div', 'room-history-meta', 'Tick ' + entry.tick));
      return item;
    }),
  );

  const form = createEl('form', 'room-trigger-form');
  form.setAttribute('data-trigger-kind', 'stimulus');
  form.addEventListener('submit', submitStimulusTrigger);

  const typeField = createEl('label', 'room-trigger-field');
  typeField.appendChild(createEl('span', 'room-trigger-label', 'Reiz-Typ'));
  const select = document.createElement('select');
  select.className = 'room-trigger-select';
  select.addEventListener('change', () => {
    const selectedValue = readSelectedValue(select);
    if (selectedValue) {
      setStimulusType(selectedValue);
      renderFloorplan(latestRooms);
    }
  });
  for (const [value, label] of STIMULUS_OPTIONS) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    option.selected = value === stimulusTriggerState.stimulusType;
    select.appendChild(option);
  }
  typeField.appendChild(select);
  form.appendChild(typeField);

  const deltaField = createEl('label', 'room-trigger-field');
  deltaField.appendChild(createEl('span', 'room-trigger-label', 'Delta'));
  const deltaInput = document.createElement('input');
  deltaInput.type = 'number';
  deltaInput.step = '0.1';
  deltaInput.className = 'room-trigger-input';
  deltaInput.value = stimulusTriggerState.delta;
  deltaInput.addEventListener('input', () => {
    stimulusTriggerState.delta = deltaInput.value;
  });
  const selectedStimulus = STIMULUS_OPTIONS.find(([value]) => value === stimulusTriggerState.stimulusType);
  deltaInput.placeholder = selectedStimulus ? String(selectedStimulus[2]) : '0';
  deltaField.appendChild(deltaInput);
  form.appendChild(deltaField);

  const durationField = createEl('label', 'room-trigger-field');
  durationField.appendChild(createEl('span', 'room-trigger-label', 'Dauer in Ticks (optional)'));
  const durationInput = document.createElement('input');
  durationInput.type = 'number';
  durationInput.min = '1';
  durationInput.step = '1';
  durationInput.placeholder = 'z. B. 120';
  durationInput.className = 'room-trigger-input';
  durationInput.value = stimulusTriggerState.durationTicks;
  durationInput.addEventListener('input', () => {
    stimulusTriggerState.durationTicks = durationInput.value;
  });
  durationField.appendChild(durationInput);
  form.appendChild(durationField);

  const submit = createEl(
    'button',
    'room-trigger-submit',
    stimulusTriggerState.pending ? 'Reiz laeuft...' : 'Raumreiz ausloesen',
  );
  submit.type = 'submit';
  submit.disabled = stimulusTriggerState.pending || !getApiKey();
  form.appendChild(submit);

  if (stimulusTriggerState.message) {
    form.appendChild(
      createEl(
        'div',
        stimulusTriggerState.error ? 'room-trigger-feedback error' : 'room-trigger-feedback',
        stimulusTriggerState.message,
      ),
    );
  }

  section.appendChild(form);
  return section;
}

function renderChaosTriggerSection() {
  const section = createEl('section', 'room-detail-section');
  section.appendChild(createEl('h3', 'room-detail-heading', 'Chaos triggern'));

  if (!getApiKey()) {
    const authBox = createEl('div', 'room-detail-auth');
    authBox.appendChild(
      createEl('div', 'room-detail-note', 'API-Key eingeben um Chaos zu triggern:'),
    );
    const keyInput = document.createElement('input');
    keyInput.type = 'password';
    keyInput.className = 'room-trigger-input';
    keyInput.placeholder = 'Operator API-Key';
    keyInput.addEventListener('change', () => {
      if (keyInput.value.trim()) {
        sessionStorage.setItem('sentinel_api_key', keyInput.value.trim());
        renderFloorplan(latestRooms);
      }
    });
    authBox.appendChild(keyInput);
    section.appendChild(authBox);
  }

  const form = createEl('form', 'room-trigger-form');
  form.setAttribute('data-trigger-kind', 'chaos');
  form.addEventListener('submit', submitChaosTrigger);

  const typeField = createEl('label', 'room-trigger-field');
  typeField.appendChild(createEl('span', 'room-trigger-label', 'Chaos-Typ'));
  const select = document.createElement('select');
  select.className = 'room-trigger-select';
  select.addEventListener('change', () => {
    const selectedValue = readSelectedValue(select);
    if (selectedValue) {
      chaosTriggerState.chaosType = selectedValue;
    }
  });
  for (const [value, label] of CHAOS_OPTIONS) {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    option.selected = value === chaosTriggerState.chaosType;
    select.appendChild(option);
  }
  typeField.appendChild(select);
  form.appendChild(typeField);

  const durationField = createEl('label', 'room-trigger-field');
  durationField.appendChild(createEl('span', 'room-trigger-label', 'Dauer in Ticks (optional)'));
  const duration = document.createElement('input');
  duration.type = 'number';
  duration.min = '1';
  duration.step = '1';
  duration.placeholder = 'z. B. 120';
  duration.className = 'room-trigger-input';
  duration.value = chaosTriggerState.durationTicks;
  duration.addEventListener('input', () => {
    chaosTriggerState.durationTicks = duration.value;
  });
  durationField.appendChild(duration);
  form.appendChild(durationField);

  const submit = createEl(
    'button',
    'room-trigger-submit',
    chaosTriggerState.pending ? 'Trigger laeuft...' : 'Chaos ausloesen',
  );
  submit.type = 'submit';
  submit.disabled = chaosTriggerState.pending || !getApiKey();
  form.appendChild(submit);

  if (chaosTriggerState.message) {
    form.appendChild(
      createEl(
        'div',
        chaosTriggerState.error ? 'room-trigger-feedback error' : 'room-trigger-feedback',
        chaosTriggerState.message,
      ),
    );
  }

  section.appendChild(form);
  return section;
}

function renderRoomDrawer(container) {
  if (!activeRoomId) return;

  const backdrop = createEl('button', 'room-detail-backdrop');
  backdrop.type = 'button';
  backdrop.setAttribute('aria-label', 'Raumdetail schliessen');
  backdrop.addEventListener('click', closeRoomDetail);
  container.appendChild(backdrop);

  const drawer = createEl('aside', 'room-detail-drawer');
  drawer.setAttribute('role', 'dialog');
  drawer.setAttribute('aria-modal', 'true');
  drawer.setAttribute('aria-labelledby', 'room-detail-title');

  const header = createEl('div', 'room-detail-header');
  const titleWrap = createEl('div', 'room-detail-titlewrap');
  const activeRoom = latestRooms.find((room) => room.id === activeRoomId);
  titleWrap.appendChild(
    createEl(
      'h2',
      'room-detail-title',
      activeRoom ? activeRoom.name : activeRoomId,
    ),
  );
  if (activeRoom) {
    titleWrap.appendChild(
      createEl(
        'div',
        'room-detail-subtitle',
        'Typ: ' + activeRoom.room_type + ' | Stockwerk: ' + activeRoom.floor,
      ),
    );
  }
  header.appendChild(titleWrap);

  const closeBtn = createEl('button', 'room-detail-close', 'Schliessen');
  closeBtn.type = 'button';
  closeBtn.addEventListener('click', closeRoomDetail);
  header.appendChild(closeBtn);
  drawer.appendChild(header);

  if (detailLoading) {
    drawer.appendChild(createEl('div', 'room-detail-state', 'Detaildaten werden geladen...'));
  } else if (detailError) {
    drawer.appendChild(createEl('div', 'room-detail-state error', detailError));
  } else if (activeRoomDetail) {
    drawer.appendChild(renderDetailSnapshot(activeRoomDetail));
    drawer.appendChild(renderPhysicsSection(activeRoomDetail));
    drawer.appendChild(renderStimulusSection(activeRoomDetail));
    drawer.appendChild(renderChaosSection(activeRoomDetail));
    drawer.appendChild(renderReactionSection(activeRoomDetail));
    drawer.appendChild(renderChaosTriggerSection());
  }

  container.appendChild(drawer);
}

export function renderFloorplan(rooms) {
  latestRooms = Array.isArray(rooms) ? rooms : latestRooms;

  const container = getFloorplanContainer();
  if (!container) return;
  while (container.firstChild) container.removeChild(container.firstChild);

  const layout = createEl('div', 'floorplan-layout');
  const wrapper = createEl('div', 'floorplan-container');

  const floors = new Map();
  for (const room of latestRooms) {
    if (!floors.has(room.floor)) floors.set(room.floor, []);
    floors.get(room.floor).push(room);
  }

  const sortedFloors = [...floors.entries()].sort((a, b) => b[0] - a[0]);
  for (const [floorNum, floorRooms] of sortedFloors) {
    const floorDiv = createEl('section', 'floor');
    const floorName =
      floorNum === 1 ? 'Obergeschoss' : floorNum === 0 ? 'Erdgeschoss' : 'Treppenhaus';
    floorDiv.appendChild(createEl('h2', '', floorName));

    const roomsGrid = createEl('div', 'rooms-grid');
    for (const room of floorRooms) {
      roomsGrid.appendChild(createRoomCard(room));
    }
    floorDiv.appendChild(roomsGrid);
    wrapper.appendChild(floorDiv);
  }

  layout.appendChild(wrapper);
  container.appendChild(layout);
  renderRoomDrawer(container);
}

export function refreshActiveRoomDetail() {
  if (!activeRoomId) return;
  void loadRoomDetail(activeRoomId, { silent: true });
}
