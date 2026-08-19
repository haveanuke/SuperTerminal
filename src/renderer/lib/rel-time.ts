/** Compact relative time for graph rows: "now", "5m", "3h", "2d", "4mo", "1y". */
export function relTime(unixSecs: number, nowMs: number = Date.now()): string {
  const diff = Math.max(0, Math.floor(nowMs / 1000) - unixSecs);
  if (diff < 60) return 'now';
  const mins = Math.floor(diff / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months}mo`;
  return `${Math.floor(months / 12)}y`;
}
