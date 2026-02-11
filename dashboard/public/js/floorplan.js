export function renderFloorplan(rooms) {
  const container = document.getElementById('view-floorplan');
  while (container.firstChild) container.removeChild(container.firstChild);

  const wrapper = document.createElement('div');
  wrapper.className = 'floorplan-container';

  // Gruppiere nach Floor
  const floors = new Map();
  for (const room of rooms) {
    const floorKey = room.floor;
    if (!floors.has(floorKey)) floors.set(floorKey, []);
    floors.get(floorKey).push(room);
  }

  // Sortiert anzeigen: OG (1), EG (0), Treppenhaus (-1)
  const sortedFloors = [...floors.entries()].sort((a, b) => b[0] - a[0]);

  for (const [floorNum, floorRooms] of sortedFloors) {
    const floorDiv = document.createElement('div');
    floorDiv.className = 'floor';

    const h2 = document.createElement('h2');
    const floorName = floorNum === 1 ? 'Obergeschoss' : floorNum === 0 ? 'Erdgeschoss' : 'Treppenhaus';
    h2.textContent = floorName;
    floorDiv.appendChild(h2);

    const roomsGrid = document.createElement('div');
    roomsGrid.className = 'rooms-grid';

    for (const room of floorRooms) {
      roomsGrid.appendChild(createRoomCard(room));
    }

    floorDiv.appendChild(roomsGrid);
    wrapper.appendChild(floorDiv);
  }

  container.appendChild(wrapper);
}

function createRoomCard(room) {
  const card = document.createElement('div');
  card.className = 'room-card';

  const h4 = document.createElement('h4');
  h4.textContent = room.name;
  card.appendChild(h4);

  const type = document.createElement('div');
  type.className = 'room-type';
  type.textContent = room.room_type;
  card.appendChild(type);

  if (room.occupants && room.occupants.length > 0) {
    const occ = document.createElement('div');
    occ.className = 'room-occupants';
    for (const name of room.occupants) {
      const dot = document.createElement('span');
      dot.className = 'occupant-dot';
      occ.appendChild(dot);
      const nameSpan = document.createElement('span');
      nameSpan.textContent = name + ' ';
      occ.appendChild(nameSpan);
    }
    card.appendChild(occ);
  }

  const env = document.createElement('div');
  env.className = 'room-env';
  env.textContent = room.temperature + '\u00B0C | ' + room.noise_db + 'dB | ' + room.co2_ppm + 'ppm';
  card.appendChild(env);

  return card;
}
