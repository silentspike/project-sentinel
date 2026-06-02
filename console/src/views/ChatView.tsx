import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { postJson } from "../api";
import { VirtualScroller } from "../components/VirtualScroller";
import { roomDisplayName } from "../roomsMeta";
import { consoleStore, type EventLogRow } from "../stores/console";
import { formatDateTime } from "./format";
import { toChatMessage, type ChatMessage } from "./eventModel";

function ChatBubble(props: { message: ChatMessage }): JSX.Element {
  const kind = () => props.message.agent_id === "operator"
    ? "operator"
    : props.message.agent_id === "gateway"
      ? "gateway"
      : "agent";
  return (
    <article class={`chat-message chat-message--${kind()}`} data-testid="chat-message" data-room={props.message.target_room ?? ""}>
      <div class="chat-message__head">
        <strong>{props.message.agent_name}</strong>
        <span>{props.message.action_type || "message"}</span>
      </div>
      <p>{props.message.content}</p>
      <div class="chat-message__meta">
        <span>{roomDisplayName(props.message.target_room)}</span>
        <span>{props.message.tick > 0 ? `T${props.message.tick}` : formatDateTime(props.message.timestamp_ms)}</span>
      </div>
    </article>
  );
}

function eventMessages(events: readonly EventLogRow[]): ChatMessage[] {
  return events
    .map((event) => toChatMessage(event, consoleStore.agents))
    .filter((message): message is ChatMessage => message !== null);
}

export function ChatView(): JSX.Element {
  const [room, setRoom] = createSignal<string | null>(null);
  const [message, setMessage] = createSignal("");
  const [sending, setSending] = createSignal(false);
  const [feedback, setFeedback] = createSignal<string | null>(null);
  const [localMessages, setLocalMessages] = createSignal<ChatMessage[]>([]);

  const allMessages = createMemo(() => [...eventMessages(consoleStore.events), ...localMessages()]
    .sort((a, b) => b.timestamp_ms - a.timestamp_ms)
    .slice(0, 10_000));

  const roomIds = createMemo(() => {
    const ids = new Set<string>();
    for (const row of consoleStore.rooms) ids.add(row.room_id);
    for (const msg of allMessages()) if (msg.target_room) ids.add(msg.target_room);
    return [...ids].sort((a, b) => roomDisplayName(a).localeCompare(roomDisplayName(b), "de"));
  });

  const visibleMessages = createMemo(() => {
    const selected = room();
    return selected ? allMessages().filter((msg) => msg.target_room === selected) : allMessages();
  });

  const send = async () => {
    const text = message().trim();
    if (!text || sending()) return;
    const selectedRoom = room();
    setSending(true);
    setFeedback(null);
    setMessage("");
    try {
      const response = await postJson<Record<string, unknown>>("/api/operator/chat", {
        message: text,
        room: selectedRoom,
      });
      const now = Date.now();
      const echoes: ChatMessage[] = [{
        id: -now,
        event_id: `operator-${now}`,
        agent_id: "operator",
        agent_name: "Operator",
        action_type: "operator_message",
        content: String(response.message ?? text),
        target_room: typeof response.room === "string" ? response.room : selectedRoom,
        tick: 0,
        timestamp_ms: now,
      }];
      if (typeof response.gateway_content === "string" && response.gateway_content.length > 0) {
        echoes.push({
          id: -(now + 1),
          event_id: `gateway-${now}`,
          agent_id: "gateway",
          agent_name: "Agent (Gateway)",
          action_type: "gateway_response",
          content: response.gateway_content,
          target_room: typeof response.room === "string" ? response.room : selectedRoom,
          tick: 0,
          timestamp_ms: now + 1,
        });
      }
      setLocalMessages((rows) => [...echoes, ...rows].slice(0, 200));
      setFeedback("Gesendet");
    } catch (error) {
      setMessage(text);
      setFeedback(error instanceof Error ? error.message : "Senden fehlgeschlagen");
    } finally {
      setSending(false);
    }
  };

  return (
    <section class="col view-panel" data-testid="view-chat">
      <div class="col__head view-head">
        <span>Chat</span>
        <span class="pill" data-testid="chat-count">{visibleMessages().length} Nachrichten</span>
      </div>
      <div class="col__body chat-shell">
        <div class="segmented-control segmented-control--wrap" data-testid="chat-room-filter">
          <button class={room() === null ? "active" : ""} onClick={() => setRoom(null)}>Alle</button>
          <For each={roomIds()}>
            {(roomId) => (
              <button class={room() === roomId ? "active" : ""} onClick={() => setRoom(roomId)}>
                {roomDisplayName(roomId)}
              </button>
            )}
          </For>
        </div>
        <div class="chat-list" data-testid="chat-list">
          <Show
            when={visibleMessages().length > 0}
            fallback={<div class="event-empty">Keine Nachrichten vorhanden</div>}
          >
            <VirtualScroller
              items={visibleMessages()}
              rowHeight={112}
              height={Math.min(640, Math.max(320, visibleMessages().length * 112))}
              overscan={8}
              renderRow={(msg) => <ChatBubble message={msg} />}
            />
          </Show>
        </div>
        <div class="chat-input-row">
          <input
            data-testid="chat-input"
            value={message()}
            placeholder={room() ? `Nachricht an ${roomDisplayName(room())}` : "Nachricht an Gaia"}
            disabled={sending()}
            onInput={(event) => setMessage(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void send();
            }}
          />
          <button class="primary" data-testid="chat-send" disabled={sending() || message().trim().length === 0} onClick={() => void send()}>
            Senden
          </button>
        </div>
        <Show when={feedback()}>
          {(text) => <div class={`trigger-feedback ${text() === "Gesendet" ? "trigger-feedback--ok" : "trigger-feedback--error"}`}>{text()}</div>}
        </Show>
      </div>
    </section>
  );
}
