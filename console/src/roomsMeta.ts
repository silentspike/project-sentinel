import type { AgentRow, RoomRow } from "./stores/console";

export interface RoomMeta {
  id: string;
  name: string;
  floor: number;
  capacity: number;
  room_type: string;
  department?: string;
}

export interface RoomViewModel extends RoomRow, RoomMeta {
  id: string;
  occupants: string[];
}

export const ROOM_METADATA: Record<string, RoomMeta> = {
  empfang: { id: "empfang", name: "Empfang", floor: 0, capacity: 4, room_type: "common" },
  "flur-eg": { id: "flur-eg", name: "Flur Erdgeschoss", floor: 0, capacity: 20, room_type: "transit" },
  kueche: { id: "kueche", name: "Küche / Pausenraum", floor: 0, capacity: 10, room_type: "break" },
  "buero-dev-1": { id: "buero-dev-1", name: "Entwicklungsbüro 1", floor: 0, capacity: 6, room_type: "office", department: "Entwicklung" },
  "buero-dev-2": { id: "buero-dev-2", name: "Entwicklungsbüro 2", floor: 0, capacity: 6, room_type: "office", department: "Entwicklung" },
  "meetingraum-01": { id: "meetingraum-01", name: "Meetingraum Galileo", floor: 0, capacity: 8, room_type: "meeting" },
  "toilette-eg-damen": { id: "toilette-eg-damen", name: "Toilette EG Damen", floor: 0, capacity: 6, room_type: "bathroom" },
  "toilette-eg-herren": { id: "toilette-eg-herren", name: "Toilette EG Herren", floor: 0, capacity: 6, room_type: "bathroom" },
  treppenhaus: { id: "treppenhaus", name: "Treppenhaus", floor: -1, capacity: 5, room_type: "transit" },
  "flur-og": { id: "flur-og", name: "Flur Obergeschoss", floor: 1, capacity: 20, room_type: "transit" },
  "buero-design-1": { id: "buero-design-1", name: "Designbüro 1", floor: 1, capacity: 6, room_type: "office", department: "Design" },
  "buero-design-2": { id: "buero-design-2", name: "Designbüro 2", floor: 1, capacity: 6, room_type: "office", department: "Design" },
  "buero-ceo": { id: "buero-ceo", name: "Geschäftsführung", floor: 1, capacity: 4, room_type: "office", department: "Geschäftsführung" },
  "meetingraum-02": { id: "meetingraum-02", name: "Meetingraum Tesla", floor: 1, capacity: 12, room_type: "meeting" },
  "meetingraum-03": { id: "meetingraum-03", name: "Meetingraum Edison", floor: 1, capacity: 6, room_type: "meeting" },
  "toilette-og-damen": { id: "toilette-og-damen", name: "Toilette OG Damen", floor: 1, capacity: 6, room_type: "bathroom" },
  "toilette-og-herren": { id: "toilette-og-herren", name: "Toilette OG Herren", floor: 1, capacity: 6, room_type: "bathroom" },
  "buero-sales": { id: "buero-sales", name: "Vertriebsbüro", floor: 0, capacity: 4, room_type: "office", department: "Vertrieb" },
  "buero-pm": { id: "buero-pm", name: "Projektmanagement-Büro", floor: 0, capacity: 10, room_type: "office", department: "Projektmanagement" },
  "buero-marketing": { id: "buero-marketing", name: "Marketingbüro", floor: 0, capacity: 4, room_type: "office", department: "Marketing" },
  "buero-admin": { id: "buero-admin", name: "Verwaltungsbüro", floor: 0, capacity: 8, room_type: "office", department: "Verwaltung" },
  "buero-qa": { id: "buero-qa", name: "QA-Büro", floor: 0, capacity: 4, room_type: "office", department: "Qualitätssicherung" },
  "buero-it": { id: "buero-it", name: "IT-Büro", floor: 0, capacity: 2, room_type: "office", department: "IT" },
  "buero-betriebsrat": { id: "buero-betriebsrat", name: "Betriebsratsbüro", floor: 1, capacity: 4, room_type: "office", department: "Betriebsrat" },
  "buero-betriebspsych": { id: "buero-betriebspsych", name: "Betriebspsychologie", floor: 1, capacity: 4, room_type: "office", department: "Gesundheit" },
  "buero-betriebsarzt": { id: "buero-betriebsarzt", name: "Betriebsmedizin", floor: 1, capacity: 4, room_type: "office", department: "Gesundheit" },
};

export function roomDisplayName(roomId: string | null | undefined): string {
  if (!roomId) return "—";
  return ROOM_METADATA[roomId]?.name ?? roomId;
}

export function mergeRoomMeta(room: RoomRow, agents: AgentRow[]): RoomViewModel {
  const meta = ROOM_METADATA[room.room_id] ?? {
    id: room.room_id,
    name: room.room_id,
    floor: 0,
    capacity: 0,
    room_type: "unknown",
  };
  return {
    ...room,
    ...meta,
    id: room.room_id,
    occupants: agents.filter((agent) => agent.current_room === room.room_id).map((agent) => agent.name),
  };
}
