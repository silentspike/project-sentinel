import { Hono } from "hono";
import { getRecentChatMessages, getChatMessagesByRoom, insertOperatorMessage } from "../db";

const CORTEX_GATEWAY_URL = process.env.CORTEX_GATEWAY_URL || "http://localhost:8080";

export const chatRoutes = new Hono();

// POST /chat — Operator sends a message to agents via Cortex Gateway
chatRoutes.post("/chat", async (c) => {
  let body: { message: string; room?: string | null };
  try {
    body = await c.req.json();
  } catch {
    return c.json({ error: "invalid JSON body" }, 400);
  }
  if (!body.message || body.message.trim().length === 0) {
    return c.json({ error: "message required" }, 400);
  }

  const message = body.message.trim();
  const room = body.room || null;

  // 1. Persist operator message as event
  const eventId = insertOperatorMessage(message, room);

  // 2. Forward to Cortex Gateway for LLM processing
  let gatewayResponse = null;
  try {
    const gatewayReq = {
      messages: [
        { role: "system", content: `Du bist ein Mitarbeiter der PixelPerfekt GmbH. Der Operator hat dir eine Nachricht geschickt.${room ? ` Du befindest dich im Raum: ${room}.` : ""}` },
        { role: "user", content: message },
      ],
      metadata: {
        source: "operator_chat",
        room: room,
      },
    };
    const resp = await fetch(`${CORTEX_GATEWAY_URL}/v1/chat/completions`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(gatewayReq),
      signal: AbortSignal.timeout(30000),
    });
    if (resp.ok) {
      gatewayResponse = await resp.json();
    } else {
      const errorText = await resp.text().catch(() => "unknown");
      console.error(`Gateway error ${resp.status}: ${errorText.slice(0, 200)}`);
      gatewayResponse = { error: `Gateway ${resp.status}`, detail: errorText.slice(0, 200) };
    }
  } catch (e) {
    console.error("Gateway unavailable:", e);
    gatewayResponse = { error: "Gateway unavailable" };
  }

  return c.json({
    event_id: eventId,
    message,
    room,
    gateway_response: gatewayResponse,
  });
});

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
