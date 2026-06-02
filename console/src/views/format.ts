export function asNumber(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

export function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

export function percentValue(value: number | undefined | null): number {
  if (value == null || Number.isNaN(value)) return 0;
  const scaled = value <= 1 && value >= 0 ? value * 100 : value;
  return Math.min(100, Math.max(0, Math.round(scaled)));
}

export function formatNumber(value: number | null | undefined): string {
  if (value == null) return "0";
  if (Math.abs(value) >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (Math.abs(value) >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return String(Math.round(value));
}

export function formatMs(value: number | null | undefined): string {
  if (value == null) return "0 ms";
  if (value >= 1000) return `${(value / 1000).toFixed(2)} s`;
  if (value > 0 && value < 1) return `${value.toFixed(2)} ms`;
  return `${Math.round(value)} ms`;
}

export function formatBytes(value: number | null | undefined): string {
  if (!value) return "0 B";
  if (value >= 1_073_741_824) return `${(value / 1_073_741_824).toFixed(1)} GB`;
  if (value >= 1_048_576) return `${(value / 1_048_576).toFixed(1)} MB`;
  if (value >= 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${value} B`;
}

export function formatMetric(value: number | null | undefined, suffix: string, digits = 0): string {
  if (value == null) return "n/a";
  return `${digits > 0 ? value.toFixed(digits) : Math.round(value)} ${suffix}`;
}

export function formatDateTime(timestampMs: number | null | undefined): string {
  if (!timestampMs) return "—";
  return new Date(timestampMs).toLocaleString("de-DE");
}

export function formatBucket(value: number | null | undefined): string {
  if (!value) return "n/a";
  if (value > 1_000_000_000_000) return new Date(value).toLocaleString("de-DE");
  return `t${value}`;
}
