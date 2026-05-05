export function formatPolledAt(s: string | null | undefined): string {
  if (s === null || s === undefined || s === "") return "never polled";
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return "never polled";
  return `last polled ${d.toLocaleString()}`;
}
