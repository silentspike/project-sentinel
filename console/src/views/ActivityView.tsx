import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { VirtualScroller } from "../components/VirtualScroller";
import { consoleStore } from "../stores/console";
import {
  ACTIVITY_FILTERS, activityItem, type ActivityFilterKey, type ActivityItem,
} from "./eventModel";
import { roomDisplayName } from "../roomsMeta";

function EventRow(props: { item: ActivityItem }): JSX.Element {
  return (
    <article class={`event-row event-row--${props.item.tone}`} data-testid="activity-event" data-event-type={props.item.event.event_type}>
      <span class={`event-badge event-badge--${props.item.tone}`}>{props.item.badge}</span>
      <div class="event-main">
        <strong>{props.item.summary}</strong>
        <Show when={props.item.detail}>
          {(detail) => <span class="event-detail">{detail()}</span>}
        </Show>
      </div>
      <Show when={props.item.room}>
        {(room) => <span class="event-room">{roomDisplayName(room())}</span>}
      </Show>
      <span class="event-meta">T{props.item.event.tick}</span>
    </article>
  );
}

export function ActivityView(): JSX.Element {
  const [mode, setMode] = createSignal<ActivityFilterKey>("all");
  const [query, setQuery] = createSignal("");

  const items = createMemo(() => consoleStore.events
    .map((event) => activityItem(event, consoleStore.agents))
    .reverse());

  const filtered = createMemo(() => {
    const selected = ACTIVITY_FILTERS.find((filter) => filter.key === mode());
    const q = query().trim().toLowerCase();
    return items().filter((item) => {
      if (selected?.types && !(selected.types as readonly string[]).includes(item.event.event_type)) {
        return false;
      }
      return q.length === 0 || item.searchText.includes(q);
    });
  });

  const countLabel = createMemo(() => {
    const total = items().length;
    const shown = filtered().length;
    return shown === total ? `${total} Events` : `${shown} / ${total} Events`;
  });

  return (
    <section class="col view-panel" data-testid="view-activity">
      <div class="col__head view-head">
        <span>Aktivitaeten</span>
        <span class="pill" data-testid="activity-count">{countLabel()}</span>
      </div>
      <div class="col__body view-body">
        <div class="view-toolbar">
          <div class="segmented-control" data-testid="activity-filters">
            <For each={ACTIVITY_FILTERS}>
              {(filter) => (
                <button
                  class={mode() === filter.key ? "active" : ""}
                  data-testid={`activity-filter-${filter.key}`}
                  onClick={() => setMode(filter.key)}
                >
                  {filter.label}
                </button>
              )}
            </For>
          </div>
          <input
            data-testid="activity-search"
            type="search"
            placeholder="Agent, Raum oder Text filtern"
            value={query()}
            onInput={(event) => setQuery(event.currentTarget.value)}
          />
        </div>
        <Show
          when={filtered().length > 0}
          fallback={<div class="event-empty">{items().length === 0 ? "Keine Aktivitaeten vorhanden" : "Keine passenden Aktivitaeten fuer den aktuellen Filter"}</div>}
        >
          <VirtualScroller
            items={filtered()}
            rowHeight={76}
            height={Math.min(640, Math.max(320, filtered().length * 76))}
            overscan={8}
            renderRow={(item) => <EventRow item={item} />}
          />
        </Show>
      </div>
    </section>
  );
}
