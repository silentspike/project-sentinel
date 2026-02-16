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
  card.setAttribute('data-room-id', room.id);

  const h4 = document.createElement('h4');
  h4.textContent = room.name;
  card.appendChild(h4);

  const type = document.createElement('div');
  type.className = 'room-type';
  type.textContent = room.room_type;
  card.appendChild(type);

  // Belegung
  const occ = document.createElement('div');
  occ.className = 'room-occupancy';
  occ.textContent = room.occupant_count + ' Personen';
  if (room.occupant_count > 0) occ.classList.add('occupied');
  card.appendChild(occ);

  // Transit
  if (room.transit_count > 0) {
    const transit = document.createElement('div');
    transit.className = 'transit-indicator';
    transit.textContent = room.transit_count + ' unterwegs';
    card.appendChild(transit);
  }

  // Chaos
  if (room.active_chaos) {
    const chaos = document.createElement('div');
    chaos.className = 'chaos-badge';
    const chaosData = typeof room.active_chaos === 'object' ? room.active_chaos : null;
    chaos.textContent = chaosData ? (chaosData.type || 'Chaos') : 'Chaos aktiv';
    card.appendChild(chaos);
  }

  return card;
}
