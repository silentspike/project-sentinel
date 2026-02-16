import { Hono } from "hono";
import { getAllRooms, getRoom } from "../db";
import { ROOM_METADATA } from "../rooms-meta";
import type { RoomRow, RoomResponse } from "../types";

export const roomRoutes = new Hono();

function toResponse(row: RoomRow): RoomResponse {
  const meta = ROOM_METADATA[row.room_id];
  let chaos: unknown | null = null;
  if (row.active_chaos) {
    try {
      chaos = JSON.parse(row.active_chaos);
    } catch {
      chaos = null;
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
    last_event_tick: row.last_event_tick,
  };
}

roomRoutes.get("/rooms", (c) => {
  const rooms = getAllRooms();
  return c.json(rooms.map(toResponse));
});

roomRoutes.get("/rooms/:id", (c) => {
  const roomId = c.req.param("id");
  const room = getRoom(roomId);
  if (!room) return c.json({ error: "Room not found" }, 404);
  return c.json(toResponse(room));
});
