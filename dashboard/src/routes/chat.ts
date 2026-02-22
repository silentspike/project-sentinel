import { Hono } from "hono";
import { getRecentChatMessages, getChatMessagesByRoom } from "../db";

export const chatRoutes = new Hono();

chatRoutes.get("/chat", (c) => {
  const limit = Math.min(
    Math.max(parseInt(c.req.query("limit") || "100", 10), 1),
    500,
  );
  return c.json(getRecentChatMessages(limit));
});

chatRoutes.get("/chat/:room", (c) => {
  const roomId = c.req.param("room");
  const limit = Math.min(
    Math.max(parseInt(c.req.query("limit") || "50", 10), 1),
    200,
  );
  return c.json(getChatMessagesByRoom(roomId, limit));
});
