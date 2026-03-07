import { Hono } from "hono";
import { getAllRooms, getRoom, getOccupantsByRoom } from "../db";
import { ROOM_METADATA } from "../rooms-meta";
import type { RoomRow, RoomResponse } from "../types";

export const roomRoutes = new Hono();

function toResponse(row: RoomRow, occupants: string[]): RoomResponse {
  const meta = ROOM_METADATA[row.room_id];
  let chaos: unknown | null = null;
  if (row.active_chaos) {
    try {
      chaos = JSON.parse(row.active_chaos);
    } catch {
      chaos = null;
    }
  }
  let smells: unknown | null = null;
  if (row.active_smells) {
    try {
      smells = JSON.parse(row.active_smells);
    } catch {
      smells = null;
    }
  }
  return {
    id: row.room_id,
    name: meta?.name ?? row.room_id,
    floor: meta?.floor ?? 0,
    capacity: meta?.capacity ?? 0,
    room_type: meta?.room_type ?? "unknown",
    occupant_count: row.occupant_count,
    transit_count: row.transit_count,
    active_chaos: chaos,
    active_smells: smells,
    temperature: row.temperature,
    co2_ppm: row.co2_ppm,
    noise_db: row.noise_db,
    last_event_tick: row.last_event_tick,
    occupants,
  };
}

roomRoutes.get("/rooms", (c) => {
  const rooms = getAllRooms();
  const occupantsMap = getOccupantsByRoom();
  return c.json(rooms.map((r) => toResponse(r, occupantsMap[r.room_id] ?? [])));
});

roomRoutes.get("/rooms/:id", (c) => {
  const roomId = c.req.param("id");
  const room = getRoom(roomId);
  if (!room) return c.json({ error: "Room not found" }, 404);
  const occupantsMap = getOccupantsByRoom();
  return c.json(toResponse(room, occupantsMap[roomId] ?? []));
});
