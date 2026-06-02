import { createMemo, Show, type JSX } from "solid-js";
import { VirtualScroller } from "../components/VirtualScroller";
import { roomDisplayName } from "../roomsMeta";
import { consoleStore } from "../stores/console";
import { activityItem, chaosType } from "./eventModel";

function ChaosRow(props: { event: ReturnType<typeof activityItem> }): JSX.Element {
  const room = () => props.event.room ?? props.event.event.aggregate_id;
  return (
    <article class="event-row event-row--chaos" data-testid="chaos-event">
      <span class="event-badge event-badge--chaos">{chaosType(props.event.event)}</span>
      <div class="event-main">
        <strong>{roomDisplayName(room())}</strong>
        <Show when={props.event.detail}>
          {(detail) => <span class="event-detail">{detail()}</span>}
        </Show>
      </div>
      <span class="event-room">{roomDisplayName(room())}</span>
      <span class="event-meta">T{props.event.event.tick}</span>
    </article>
  );
}

export function ChaosView(): JSX.Element {
  const chaosEvents = createMemo(() => consoleStore.events
    .filter((event) => event.event_type === "chaos_triggered")
    .map((event) => activityItem(event, consoleStore.agents))
    .reverse());

  return (
    <section class="col view-panel" data-testid="view-chaos">
      <div class="col__head view-head">
        <span>Chaos</span>
        <span class="pill" data-testid="chaos-count">{chaosEvents().length} Events</span>
      </div>
      <div class="col__body view-body">
        <Show
          when={chaosEvents().length > 0}
          fallback={<div class="event-empty">Keine Chaos-Events vorhanden</div>}
        >
          <VirtualScroller
            items={chaosEvents()}
            rowHeight={76}
            height={Math.min(640, Math.max(320, chaosEvents().length * 76))}
            overscan={8}
            renderRow={(event) => <ChaosRow event={event} />}
          />
        </Show>
      </div>
    </section>
  );
}
