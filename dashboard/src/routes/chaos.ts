import { Hono } from "hono";
import { getRecentChaosEvents, getChaosEventsByRoom } from "../db";

export const chaosRoutes = new Hono();

chaosRoutes.get("/chaos", (c) => {
  const limit = Math.min(
    Math.max(parseInt(c.req.query("limit") || "100", 10), 1),
    500,
  );
  return c.json(getRecentChaosEvents(limit));
});

chaosRoutes.get("/chaos/:room", (c) => {
  const roomId = c.req.param("room");
  const limit = Math.min(
    Math.max(parseInt(c.req.query("limit") || "50", 10), 1),
    200,
  );
  return c.json(getChaosEventsByRoom(roomId, limit));
});
