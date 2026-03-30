// Events Endpoint: Durchsuchbare Event-Historie aus dem EventStore (SQLite).
// Unterstuetzt Typ-Filter, Agent-Filter, Limit und Offset.

import { Hono } from "hono";
import { eventRowSelectColumns, getEventStoreDb } from "../db";
import type { EventRow } from "../types";

export const eventRoutes = new Hono();

// ── GET /api/events — Events mit optionalem Filter ──
// Query-Parameter:
//   type    — event_type Filter (z.B. "chaos_triggered")
//   agent   — aggregate_id Filter (z.B. "AGENT-01")
//   limit   — Max Ergebnisse (default 100, max 1000)
//   offset  — Pagination Offset (default 0)
//   since   — Nur Events nach diesem Timestamp (epoch ms)

eventRoutes.get("/events", (c) => {
  const db = getEventStoreDb();
  if (!db) {
    return c.json({ error: "EventStore DB nicht verfuegbar" }, 503);
  }

  const eventType = c.req.query("type") || null;
  const agent = c.req.query("agent") || null;
  const limit = Math.min(
    Math.max(parseInt(c.req.query("limit") || "100", 10), 1),
    1000,
  );
  const offset = Math.max(parseInt(c.req.query("offset") || "0", 10), 0);
  const since = c.req.query("since")
    ? parseInt(c.req.query("since")!, 10)
    : null;

  // Build query dynamically based on filters
  const conditions: string[] = [];
  const params: (string | number)[] = [];

  if (eventType) {
    conditions.push("event_type = ?");
    params.push(eventType);
  }
  if (agent) {
    conditions.push("aggregate_id = ?");
    params.push(agent);
  }
  if (since) {
    conditions.push("timestamp_ms > ?");
    params.push(since);
  }

  const whereClause =
    conditions.length > 0 ? `WHERE ${conditions.join(" AND ")}` : "";

  const query = `SELECT ${eventRowSelectColumns()}
                 FROM events
                 ${whereClause}
                 ORDER BY id DESC
                 LIMIT ? OFFSET ?`;

  params.push(limit, offset);

  const events = db
    .query<EventRow, (string | number)[]>(query)
    .all(...params);

  // Total count for pagination
  const countQuery = `SELECT COUNT(*) as cnt FROM events ${whereClause}`;
  const countParams = params.slice(0, -2); // Remove limit/offset
  const countRow = db
    .query<{ cnt: number }, (string | number)[]>(countQuery)
    .get(...countParams);

  return c.json({
    events,
    total: countRow?.cnt ?? 0,
    limit,
    offset,
  });
});

// ── GET /api/events/types — Verfuegbare Event-Typen ──

eventRoutes.get("/events/types", (c) => {
  const db = getEventStoreDb();
  if (!db) {
    return c.json({ error: "EventStore DB nicht verfuegbar" }, 503);
  }

  const types = db
    .query<{ event_type: string; cnt: number }, []>(
      `SELECT event_type, COUNT(*) as cnt
       FROM events
       GROUP BY event_type
       ORDER BY cnt DESC`,
    )
    .all();

  return c.json({ types });
});
