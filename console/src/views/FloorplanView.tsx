import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { apiJson, postJson } from "../api";
import { consoleStore } from "../stores/console";
import { mergeRoomMeta, roomDisplayName, type RoomViewModel } from "../roomsMeta";
import { asNumber, asString, formatMetric } from "./format";

const CHAOS_OPTIONS = [
  ["AirConBroken", "Klimaanlage defekt"],
  ["PrinterBroken", "Drucker defekt"],
  ["PhoneRing", "Telefon klingelt"],
  ["PackageDelivery", "Paketlieferung"],
  ["SBahnDelay", "S-Bahn Verspätung"],
  ["FireAlarmDrill", "Feueralarm-Übung"],
  ["CakeInKitchen", "Kuchen in der Küche"],
  ["InternetOutage", "Internetausfall"],
] as const;

const STIMULUS_OPTIONS = [
  ["temperature", "Temperatur", "4"],
  ["noise", "Lärm", "24"],
  ["co2", "CO2", "900"],
] as const;

type RoomDetail = RoomViewModel & {
  physics_history?: Record<string, unknown>[];
  chaos_history?: Record<string, unknown>[];
  stimulus_history?: Record<string, unknown>[];
  recent_reactions?: Record<string, unknown>[];
  reaction_window_ticks?: number;
};

function activeChaosLabel(value: unknown): string | null {
  if (!value) return null;
  if (Array.isArray(value)) return value.length > 0 ? "Chaos aktiv" : null;
  if (typeof value === "object") {
    const object = value as Record<string, unknown>;
    return asString(object.description) || asString(object.event_type) || asString(object.type) || "Chaos aktiv";
  }
  return String(value);
}

function occupantNames(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => {
      if (typeof item === "string") return item;
      if (item && typeof item === "object") return asString((item as Record<string, unknown>).name);
      return "";
    })
    .filter(Boolean);
}

function history(detail: RoomDetail | null, key: keyof RoomDetail): Record<string, unknown>[] {
  const value = detail?.[key];
  return Array.isArray(value) ? value : [];
}

function perceptionHints(detail: RoomDetail): string[] {
  const hints: string[] = [];
  if (detail.temperature != null) {
    if (detail.temperature > 25) hints.push(`Agents spüren deutliche Wärme (${detail.temperature.toFixed(1)} °C)`);
    else if (detail.temperature > 22.5) hints.push(`Agents spüren leichte Wärme (${detail.temperature.toFixed(1)} °C)`);
    else if (detail.temperature < 19) hints.push(`Agents frieren leicht (${detail.temperature.toFixed(1)} °C)`);
  }
  if (detail.co2_ppm != null) {
    if (detail.co2_ppm > 1000) hints.push(`Agents bemerken stickige Luft (${Math.round(detail.co2_ppm)} ppm)`);
    else if (detail.co2_ppm > 600) hints.push(`Agents spüren leicht verbrauchte Luft (${Math.round(detail.co2_ppm)} ppm)`);
  }
  if (detail.noise_db != null) {
    if (detail.noise_db > 65) hints.push(`Agents empfinden den Raum als laut (${Math.round(detail.noise_db)} dB)`);
    else if (detail.noise_db > 50) hints.push(`Agents hören lebhafte Unterhaltungen (${Math.round(detail.noise_db)} dB)`);
    else if (detail.noise_db > 40) hints.push(`Agents nehmen Hintergrundgeräusche wahr (${Math.round(detail.noise_db)} dB)`);
  }
  if (detail.occupant_count > 3) hints.push(`Agents nehmen belebten Raum wahr (${detail.occupant_count} Personen)`);
  const chaos = activeChaosLabel(detail.active_chaos);
  if (chaos) hints.push(`Aktives Chaos beeinflusst Umgebung: ${chaos}`);
  return hints;
}

function HistoryList(props: {
  items: Record<string, unknown>[];
  empty: string;
  renderItem: (item: Record<string, unknown>) => JSX.Element;
}): JSX.Element {
  return (
    <Show when={props.items.length > 0} fallback={<div class="detail-empty">{props.empty}</div>}>
      <div class="history-list">
        <For each={props.items}>{props.renderItem}</For>
      </div>
    </Show>
  );
}

function RoomCard(props: { room: RoomViewModel; active: boolean; onSelect: () => void }): JSX.Element {
  const chaos = createMemo(() => activeChaosLabel(props.room.active_chaos));
  return (
    <button
      type="button"
      class={`room-card ${props.active ? "room-card--active" : ""}`}
      data-testid="room-card"
      data-room-id={props.room.id}
      aria-pressed={props.active ? "true" : "false"}
      onClick={props.onSelect}
    >
      <span class="room-card__title">{props.room.name}</span>
      <span class="room-card__type">{props.room.room_type.toUpperCase()}</span>
      <span class={`room-card__occupancy ${props.room.occupant_count > 0 ? "room-card__occupancy--active" : ""}`}>
        {props.room.occupant_count}/{props.room.capacity} Personen
      </span>
      <Show when={props.room.occupants.length > 0}>
        <span class="room-tags">
          <For each={props.room.occupants}>{(name) => <span class="room-tag">{name}</span>}</For>
        </span>
      </Show>
      <span class="room-card__physics">
        {[
          formatMetric(props.room.temperature, "°C", 1),
          formatMetric(props.room.co2_ppm, "ppm"),
          formatMetric(props.room.noise_db, "dB"),
        ].join(" | ")}
      </span>
      <Show when={props.room.transit_count > 0}>
        <span class="transit-indicator">{props.room.transit_count} unterwegs</span>
      </Show>
      <Show when={chaos()}>
        <span class="chaos-badge">{chaos()}</span>
      </Show>
      <span class="room-card__footer">Details</span>
    </button>
  );
}

export function FloorplanView(): JSX.Element {
  const [activeRoomId, setActiveRoomId] = createSignal<string | null>(null);
  const [detail, setDetail] = createSignal<RoomDetail | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [chaosType, setChaosType] = createSignal("AirConBroken");
  const [chaosDuration, setChaosDuration] = createSignal("");
  const [stimulusType, setStimulusType] = createSignal("temperature");
  const [stimulusDelta, setStimulusDelta] = createSignal("4");
  const [stimulusDuration, setStimulusDuration] = createSignal("");
  const [triggerMessage, setTriggerMessage] = createSignal<{ kind: "ok" | "error"; text: string } | null>(null);

  const rooms = createMemo(() =>
    consoleStore.rooms
      .map((room) => mergeRoomMeta(room, consoleStore.agents))
      .sort((a, b) => b.floor - a.floor || a.name.localeCompare(b.name, "de")),
  );

  const floors = createMemo(() => {
    const grouped = new Map<number, RoomViewModel[]>();
    for (const room of rooms()) {
      if (!grouped.has(room.floor)) grouped.set(room.floor, []);
      grouped.get(room.floor)?.push(room);
    }
    return [...grouped.entries()];
  });

  const activeRoom = createMemo(() => rooms().find((room) => room.id === activeRoomId()) ?? null);
  const detailOccupants = createMemo(() => occupantNames(detail()?.occupants ?? activeRoom()?.occupants ?? []));

  const loadRoomDetail = async (roomId: string, silent = false) => {
    setActiveRoomId(roomId);
    setTriggerMessage(null);
    if (!silent) {
      setDetail(null);
      setError("");
      setLoading(true);
    }
    try {
      const raw = await apiJson<Record<string, unknown>>(`/api/rooms/${encodeURIComponent(roomId)}/detail`);
      const base = rooms().find((room) => room.id === roomId);
      const occupants = occupantNames(raw.occupants ?? base?.occupants ?? []);
      const fallback: RoomViewModel = base ?? {
        room_id: roomId,
        id: roomId,
        name: roomDisplayName(roomId),
        floor: 0,
        capacity: 0,
        room_type: "unknown",
        occupant_count: asNumber(raw.occupant_count),
        transit_count: asNumber(raw.transit_count),
        active_chaos: raw.active_chaos ?? null,
        active_smells: raw.active_smells ?? null,
        temperature: typeof raw.temperature === "number" ? raw.temperature : null,
        co2_ppm: typeof raw.co2_ppm === "number" ? raw.co2_ppm : null,
        noise_db: typeof raw.noise_db === "number" ? raw.noise_db : null,
        last_event_tick: typeof raw.last_event_tick === "number" ? raw.last_event_tick : null,
        occupants,
      };
      setDetail({
        ...fallback,
        ...raw,
        id: roomId,
        name: base?.name ?? asString(raw.name, roomDisplayName(roomId)),
        floor: base?.floor ?? asNumber(raw.floor),
        capacity: base?.capacity ?? asNumber(raw.capacity),
        room_type: base?.room_type ?? asString(raw.room_type, "unknown"),
        occupants,
      } as RoomDetail);
      setError("");
    } catch (err) {
      setDetail(null);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const closeDetail = () => {
    setActiveRoomId(null);
    setDetail(null);
    setError("");
    setTriggerMessage(null);
  };

  const submitChaos = async (event: SubmitEvent) => {
    event.preventDefault();
    const roomId = activeRoomId();
    if (!roomId) return;
    const duration = Number.parseInt(chaosDuration(), 10);
    const payload: Record<string, unknown> = { room_id: roomId, chaos_type: chaosType() };
    if (Number.isFinite(duration) && duration > 0) payload.duration_ticks = duration;
    try {
      const response = await postJson<Record<string, unknown>>("/api/control/chaos", payload);
      setTriggerMessage({ kind: "ok", text: `Chaos-Trigger angenommen: ${asString(response.event_id, "ok")}` });
      setChaosDuration("");
      await loadRoomDetail(roomId, true);
    } catch (err) {
      setTriggerMessage({ kind: "error", text: err instanceof Error ? err.message : String(err) });
    }
  };

  const submitStimulus = async (event: SubmitEvent) => {
    event.preventDefault();
    const roomId = activeRoomId();
    if (!roomId) return;
    const delta = Number.parseFloat(stimulusDelta());
    if (!Number.isFinite(delta) || delta === 0) {
      setTriggerMessage({ kind: "error", text: "Bitte ein gültiges Delta ungleich 0 eingeben" });
      return;
    }
    const duration = Number.parseInt(stimulusDuration(), 10);
    const payload: Record<string, unknown> = { room_id: roomId, stimulus_type: stimulusType(), delta };
    if (Number.isFinite(duration) && duration > 0) payload.duration_ticks = duration;
    try {
      const response = await postJson<Record<string, unknown>>("/api/control/stimulus", payload);
      setTriggerMessage({ kind: "ok", text: `Raumreiz angenommen: ${asString(response.event_id, "ok")}` });
      setStimulusDuration("");
      await loadRoomDetail(roomId, true);
    } catch (err) {
      setTriggerMessage({ kind: "error", text: err instanceof Error ? err.message : String(err) });
    }
  };

  return (
    <section class="col view-panel" data-testid="view-floorplan">
      <div class="col__head view-head">
        <span>Floorplan</span>
        <span class="pill">{rooms().length} Räume</span>
      </div>
      <div class="col__body view-body floorplan-shell">
        <Show when={rooms().length > 0} fallback={<p class="muted">Warte auf room_live-Push.</p>}>
          <div class="floorplan-layout">
            <For each={floors()}>
              {([floor, floorRooms]) => (
                <section class="floor">
                  <h2>{floor === 1 ? "Obergeschoss" : floor === 0 ? "Erdgeschoss" : "Treppenhaus"}</h2>
                  <div class="rooms-grid">
                    <For each={floorRooms}>
                      {(room) => (
                        <RoomCard
                          room={room}
                          active={activeRoomId() === room.id}
                          onSelect={() => void loadRoomDetail(room.id)}
                        />
                      )}
                    </For>
                  </div>
                </section>
              )}
            </For>
          </div>
        </Show>

        <Show when={activeRoomId()}>
          <button type="button" class="drawer-backdrop" aria-label="Raumdetail schliessen" onClick={closeDetail} />
          <aside class="room-detail-drawer" role="dialog" aria-modal="true" data-testid="room-detail">
            <div class="room-detail-header">
              <div>
                <h2>{activeRoom()?.name ?? roomDisplayName(activeRoomId())}</h2>
                <p class="muted">Typ: {activeRoom()?.room_type ?? "unknown"} | Stockwerk: {activeRoom()?.floor ?? "n/a"}</p>
              </div>
              <button type="button" onClick={closeDetail}>Schliessen</button>
            </div>

            <Show when={!loading()} fallback={<div class="detail-state">Detaildaten werden geladen...</div>}>
              <Show when={!error()} fallback={<div class="detail-state detail-state--error">{error()}</div>}>
                <Show when={detail()}>
                  {(roomDetail) => (
                    <div class="room-detail-body">
                      <section class="detail-section">
                        <h3>Snapshot</h3>
                        <div class="detail-metrics">
                          <div><span>Temperatur</span><strong>{formatMetric(roomDetail().temperature, "°C", 1)}</strong></div>
                          <div><span>CO2</span><strong>{formatMetric(roomDetail().co2_ppm, "ppm")}</strong></div>
                          <div><span>Lärm</span><strong>{formatMetric(roomDetail().noise_db, "dB")}</strong></div>
                          <div><span>Belegung</span><strong>{roomDetail().occupant_count}</strong></div>
                          <div><span>Transit</span><strong>{roomDetail().transit_count}</strong></div>
                          <div><span>Letzter Tick</span><strong>{roomDetail().last_event_tick == null ? "n/a" : `t${roomDetail().last_event_tick}`}</strong></div>
                        </div>
                        <h4>Anwesende Agents</h4>
                        <Show when={detailOccupants().length > 0} fallback={<div class="detail-empty">Keine Agents im Raum</div>}>
                          <div class="room-tags"><For each={detailOccupants()}>{(name) => <span class="room-tag">{name}</span>}</For></div>
                        </Show>
                        <h4>Aktives Chaos</h4>
                        <div class={`detail-chaos ${activeChaosLabel(roomDetail().active_chaos) ? "detail-chaos--active" : ""}`}>
                          {activeChaosLabel(roomDetail().active_chaos) ?? "Kein aktives Chaos"}
                        </div>
                        <h4>Prompt-Hinweise</h4>
                        <HistoryList
                          items={perceptionHints(roomDetail()).map((hint) => ({ hint }))}
                          empty="Umgebung normal"
                          renderItem={(item) => <div class="history-item"><span>{asString(item.hint)}</span></div>}
                        />
                      </section>

                      <section class="detail-section">
                        <h3>Physics-Verlauf</h3>
                        <HistoryList
                          items={history(roomDetail(), "physics_history")}
                          empty="Noch keine Physics-Historie vorhanden"
                          renderItem={(item) => (
                            <div class="history-item">
                              <strong>t{asNumber(item.tick)}</strong>
                              <span>{[
                                formatMetric(asNumber(item.temperature), "°C", 1),
                                formatMetric(asNumber(item.co2_ppm), "ppm"),
                                formatMetric(asNumber(item.noise_db), "dB"),
                                `${asNumber(item.occupant_count)} Pers.`,
                              ].join(" | ")}</span>
                            </div>
                          )}
                        />
                      </section>

                      <section class="detail-section">
                        <h3>Raumreiz testen</h3>
                        <HistoryList
                          items={history(roomDetail(), "stimulus_history")}
                          empty="Noch keine Raumreize vorhanden"
                          renderItem={(item) => (
                            <div class="history-item">
                              <strong>{asString(item.stimulus_type)} {asNumber(item.delta) > 0 ? "+" : ""}{asNumber(item.delta)}</strong>
                              <span>{asString(item.description, "ohne Beschreibung")} | Tick {asNumber(item.tick)}</span>
                            </div>
                          )}
                        />
                        <form class="trigger-form" onSubmit={(event) => void submitStimulus(event)}>
                          <label>Reiz-Typ<select value={stimulusType()} onChange={(event) => {
                            setStimulusType(event.currentTarget.value);
                            setStimulusDelta(STIMULUS_OPTIONS.find(([value]) => value === event.currentTarget.value)?.[2] ?? "0");
                          }}>
                            <For each={STIMULUS_OPTIONS}>{([value, label]) => <option value={value}>{label}</option>}</For>
                          </select></label>
                          <label>Delta<input type="number" step="0.1" value={stimulusDelta()} onInput={(event) => setStimulusDelta(event.currentTarget.value)} /></label>
                          <label>Dauer<input type="number" min="1" step="1" placeholder="optional" value={stimulusDuration()} onInput={(event) => setStimulusDuration(event.currentTarget.value)} /></label>
                          <button type="submit" class="primary">Raumreiz auslösen</button>
                        </form>
                      </section>

                      <section class="detail-section">
                        <h3>Chaos-Historie</h3>
                        <HistoryList
                          items={history(roomDetail(), "chaos_history")}
                          empty="Noch keine Chaos-Events vorhanden"
                          renderItem={(item) => (
                            <div class="history-item history-item--chaos">
                              <strong>{asString(item.chaos_type)} in {asString(item.room_id, roomDetail().id)}</strong>
                              <span>{asString(item.description, "ohne Beschreibung")} | Tick {asNumber(item.tick)}</span>
                            </div>
                          )}
                        />
                        <form class="trigger-form" onSubmit={(event) => void submitChaos(event)}>
                          <label>Chaos-Typ<select value={chaosType()} onChange={(event) => setChaosType(event.currentTarget.value)}>
                            <For each={CHAOS_OPTIONS}>{([value, label]) => <option value={value}>{label}</option>}</For>
                          </select></label>
                          <label>Dauer<input type="number" min="1" step="1" placeholder="optional" value={chaosDuration()} onInput={(event) => setChaosDuration(event.currentTarget.value)} /></label>
                          <button type="submit" class="primary">Chaos auslösen</button>
                        </form>
                        <Show when={triggerMessage()}>
                          {(msg) => <div class={`trigger-feedback trigger-feedback--${msg().kind}`}>{msg().text}</div>}
                        </Show>
                      </section>

                      <section class="detail-section">
                        <h3>Reaktionen im Raum</h3>
                        <HistoryList
                          items={history(roomDetail(), "recent_reactions")}
                          empty="Noch keine Reaktionen im Zeitfenster"
                          renderItem={(item) => (
                            <div class="history-item history-item--reaction">
                              <strong>{asString(item.agent_name)} - {asString(item.action_type, "Aktion")}</strong>
                              <span>{asString(item.content, "ohne Details")} | Tick {asNumber(item.tick)}</span>
                            </div>
                          )}
                        />
                      </section>
                    </div>
                  )}
                </Show>
              </Show>
            </Show>
          </aside>
        </Show>
      </div>
    </section>
  );
}
