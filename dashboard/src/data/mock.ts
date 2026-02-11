export interface Agent {
  id: number;
  name: string;
  role: string;
  department: string;
  status: "active" | "sleeping" | "suspended";
  room: string;
  mood: { valence: number; arousal: number; emotion: string };
  bio: {
    hunger: number;
    energy: number;
    caffeine_mg: number;
    bladder: number;
    stress: number;
    social_need: number;
  };
}

export interface Room {
  id: string;
  name: string;
  floor: number;
  capacity: number;
  room_type: string;
  occupants: string[];
  temperature: number;
  noise_db: number;
  co2_ppm: number;
}

export interface ChatMessage {
  timestamp: string;
  agent: string;
  message: string;
  room: string;
}

export interface Metrics {
  tick_rate: number;
  agent_count: number;
  uptime: number;
  messages_per_min: number;
}

// 5 Mock-Agenten
export const mockAgents: Agent[] = [
  {
    id: 1, name: "Thomas Mueller", role: "CEO", department: "Geschaeftsfuehrung",
    status: "active", room: "buero-ceo",
    mood: { valence: 0.6, arousal: 0.4, emotion: "focused" },
    bio: { hunger: 35, energy: 72, caffeine_mg: 180, bladder: 25, stress: 30, social_need: 45 }
  },
  {
    id: 2, name: "Lisa Brenner", role: "Head of Design", department: "Design",
    status: "active", room: "buero-design-1",
    mood: { valence: 0.7, arousal: 0.3, emotion: "relaxed" },
    bio: { hunger: 20, energy: 85, caffeine_mg: 0, bladder: 15, stress: 20, social_need: 30 }
  },
  {
    id: 3, name: "Max Richter", role: "Senior UI/UX Designer", department: "Design",
    status: "active", room: "buero-design-2",
    mood: { valence: 0.5, arousal: 0.5, emotion: "focused" },
    bio: { hunger: 50, energy: 65, caffeine_mg: 90, bladder: 40, stress: 35, social_need: 55 }
  },
  {
    id: 4, name: "Sophie Klein", role: "Junior Designerin", department: "Design",
    status: "active", room: "buero-design-1",
    mood: { valence: 0.8, arousal: 0.6, emotion: "happy" },
    bio: { hunger: 15, energy: 90, caffeine_mg: 0, bladder: 10, stress: 15, social_need: 20 }
  },
  {
    id: 5, name: "Andreas Wolff", role: "Tech Lead", department: "Entwicklung",
    status: "active", room: "buero-dev-1",
    mood: { valence: 0.4, arousal: 0.3, emotion: "neutral" },
    bio: { hunger: 60, energy: 55, caffeine_mg: 120, bladder: 55, stress: 45, social_need: 70 }
  }
];

// 15 Mock-Raeume (aus rooms.toml)
export const mockRooms: Room[] = [
  { id: "empfang", name: "Empfang", floor: 0, capacity: 4, room_type: "common", occupants: [], temperature: 21.5, noise_db: 35, co2_ppm: 420 },
  { id: "flur-eg", name: "Flur Erdgeschoss", floor: 0, capacity: 20, room_type: "transit", occupants: [], temperature: 20.0, noise_db: 30, co2_ppm: 400 },
  { id: "kueche", name: "Kueche / Pausenraum", floor: 0, capacity: 10, room_type: "break", occupants: [], temperature: 22.0, noise_db: 45, co2_ppm: 550 },
  { id: "buero-dev-1", name: "Entwicklungsbuero 1", floor: 0, capacity: 6, room_type: "office", occupants: ["Andreas Wolff"], temperature: 22.5, noise_db: 42, co2_ppm: 520 },
  { id: "buero-dev-2", name: "Entwicklungsbuero 2", floor: 0, capacity: 6, room_type: "office", occupants: [], temperature: 22.0, noise_db: 38, co2_ppm: 480 },
  { id: "meetingraum-01", name: "Meetingraum Galileo", floor: 0, capacity: 8, room_type: "meeting", occupants: [], temperature: 21.0, noise_db: 30, co2_ppm: 410 },
  { id: "toilette-eg", name: "Toilette EG", floor: 0, capacity: 3, room_type: "bathroom", occupants: [], temperature: 20.0, noise_db: 25, co2_ppm: 400 },
  { id: "treppenhaus", name: "Treppenhaus", floor: -1, capacity: 5, room_type: "transit", occupants: [], temperature: 19.0, noise_db: 28, co2_ppm: 400 },
  { id: "flur-og", name: "Flur Obergeschoss", floor: 1, capacity: 20, room_type: "transit", occupants: [], temperature: 21.0, noise_db: 32, co2_ppm: 410 },
  { id: "buero-design-1", name: "Designbuero 1", floor: 1, capacity: 6, room_type: "office", occupants: ["Lisa Brenner", "Sophie Klein"], temperature: 22.0, noise_db: 38, co2_ppm: 510 },
  { id: "buero-design-2", name: "Designbuero 2", floor: 1, capacity: 6, room_type: "office", occupants: ["Max Richter"], temperature: 22.5, noise_db: 35, co2_ppm: 490 },
  { id: "buero-ceo", name: "Geschaeftsfuehrung", floor: 1, capacity: 4, room_type: "office", occupants: ["Thomas Mueller"], temperature: 22.0, noise_db: 30, co2_ppm: 450 },
  { id: "meetingraum-02", name: "Meetingraum Tesla", floor: 1, capacity: 12, room_type: "meeting", occupants: [], temperature: 21.5, noise_db: 30, co2_ppm: 420 },
  { id: "meetingraum-03", name: "Meetingraum Edison", floor: 1, capacity: 6, room_type: "meeting", occupants: [], temperature: 21.0, noise_db: 28, co2_ppm: 405 },
  { id: "toilette-og", name: "Toilette OG", floor: 1, capacity: 3, room_type: "bathroom", occupants: [], temperature: 20.5, noise_db: 25, co2_ppm: 400 }
];

// Mock-Chat
export const mockChat: ChatMessage[] = [
  { timestamp: "08:15:00", agent: "Thomas Mueller", message: "Guten Morgen zusammen!", room: "kueche" },
  { timestamp: "08:16:30", agent: "Lisa Brenner", message: "Morgen Thomas. Hast du den Entwurf gesehen?", room: "kueche" },
  { timestamp: "08:45:00", agent: "Andreas Wolff", message: "PR ist ready fuer Review.", room: "buero-dev-1" },
  { timestamp: "09:00:00", agent: "Sophie Klein", message: "Die Animation ist fertig!", room: "buero-design-1" },
  { timestamp: "09:15:00", agent: "Max Richter", message: "Usability-Test Ergebnisse sind da.", room: "buero-design-2" },
];

// Mock-Metriken
export const mockMetrics: Metrics = {
  tick_rate: 5.0,
  agent_count: 5,
  uptime: 3600,
  messages_per_min: 2.3,
};
