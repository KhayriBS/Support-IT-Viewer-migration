import type { SignalMessage } from "$lib/api";

/** Render a SignalMessage payload as a short, log-friendly string. */
export function formatSignalPayload(type: SignalMessage["type"], payload: unknown) {
  if (payload === undefined || payload === null) {
    return "";
  }

  if (type === "OFFER" || type === "ANSWER") {
    const record = payload as Record<string, unknown>;
    const sdp = typeof record?.sdp === "string" ? record.sdp : "";
    const label = typeof record?.type === "string" ? record.type : type.toLowerCase();
    return `SDP ${label} • ${sdp.length} chars`;
  }

  if (type === "ICE") {
    const record = payload as Record<string, unknown>;
    const candidate = typeof record?.candidate === "string" ? record.candidate : "";
    return candidate.length > 96 ? `${candidate.slice(0, 96)}...` : candidate || "ICE candidate";
  }

  if (type === "STREAM_STATS") {
    const record = payload as Record<string, unknown>;
    const mbps = Number(record?.mbps ?? 0);
    const fps = Number(record?.fps ?? 0);
    return `${mbps.toFixed(2)} Mbps • ${fps.toFixed(1)} FPS`;
  }

  if (type === "FILE_DATA") {
    const record = payload as Record<string, unknown>;
    if (typeof record?.chunkIndex === "number") {
      return `File chunk ${record.chunkIndex}`;
    }
    return "File data";
  }

  const payloadText = JSON.stringify(payload);
  return payloadText.length > 180 ? `${payloadText.slice(0, 180)}...` : payloadText;
}

/**
 * Retry only for transient network/server WebSocket close conditions.
 *   1006: abnormal closure (network loss)
 *   1011/1012/1013: server/internal temporary conditions
 */
export function isRetryableSignalingCloseCode(code: number) {
  return code === 1006 || code === 1011 || code === 1012 || code === 1013;
}
