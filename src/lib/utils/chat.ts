import type { ChatMessage } from "$lib/api";

/** Stable identity key. Prefers server id; falls back to content fingerprint. */
export function msgKey(msg: ChatMessage): string {
  if (msg.id !== undefined && msg.id !== null) {
    return `id:${msg.id}`;
  }
  return `${msg.senderName}:${msg.timestamp}:${msg.content.slice(0, 64)}`;
}

/** Merge two message arrays without duplicates, keeping chronological order. */
export function mergeMessages(existing: ChatMessage[], incoming: ChatMessage[]): ChatMessage[] {
  if (incoming.length === 0) return existing;
  const seen = new Set(existing.map(msgKey));
  const merged = [...existing];
  for (const msg of incoming) {
    const k = msgKey(msg);
    if (!seen.has(k)) {
      merged.push(msg);
      seen.add(k);
    }
  }
  merged.sort((a, b) => a.timestamp.localeCompare(b.timestamp));
  return merged.slice(-200);
}
