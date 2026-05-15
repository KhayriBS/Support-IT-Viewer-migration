/**
 * Diagnostic logger — always on (strip calls once issue is fixed).
 * Goal: see in DevTools console exactly which event/branch fires when the
 * session dies. Prefix every log with [DIAG] for easy grep.
 *
 * Note: we deep-clone payload via JSON to break Svelte 5 $state proxies
 * (which would otherwise trigger the `console_log_state` warning).
 */
export function diag(tag: string, payload?: unknown) {
  if (payload === undefined) {
    console.log(`[DIAG] ${tag}`);
    return;
  }
  let safe: unknown = payload;
  try {
    safe = JSON.parse(JSON.stringify(payload));
  } catch {
    // payload contains non-serializable values (Map, RTCPeerConnection, …) —
    // log as-is, the proxy warning is harmless.
  }
  console.log(`[DIAG] ${tag}`, safe);
}
