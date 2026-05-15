/**
 * Default ICE servers — Metered "global.relay" static credentials (account: lumieretech).
 * Used as a hard fallback when VITE_ICE_SERVERS is unset and the Tauri
 * get_ice_servers_cmd returns nothing usable. Rotate if Metered invalidates.
 */
export const defaultViewerIceServers: RTCIceServer[] = [
  { urls: "stun:stun.relay.metered.ca:80" },
  {
    urls: "turn:global.relay.metered.ca:80",
    username: "d156f70e60e74c734ec39dc8",
    credential: "Z9zO5Kp3c5P/c6e0"
  },
  {
    urls: "turn:global.relay.metered.ca:80?transport=tcp",
    username: "d156f70e60e74c734ec39dc8",
    credential: "Z9zO5Kp3c5P/c6e0"
  },
  {
    urls: "turn:global.relay.metered.ca:443",
    username: "d156f70e60e74c734ec39dc8",
    credential: "Z9zO5Kp3c5P/c6e0"
  },
  {
    urls: "turns:global.relay.metered.ca:443?transport=tcp",
    username: "d156f70e60e74c734ec39dc8",
    credential: "Z9zO5Kp3c5P/c6e0"
  }
];

/** Resolve viewer ICE servers from VITE_ICE_SERVERS env, or fall back to defaults. */
export function resolveIceServers(): RTCIceServer[] {
  const env = (import.meta as unknown as { env?: Record<string, unknown> }).env ?? {};
  const raw = typeof env.VITE_ICE_SERVERS === "string" ? env.VITE_ICE_SERVERS.trim() : "";

  if (!raw) {
    return defaultViewerIceServers;
  }

  if (raw.startsWith("[")) {
    try {
      const parsed = JSON.parse(raw) as unknown;
      if (Array.isArray(parsed)) {
        return parsed as RTCIceServer[];
      }
    } catch {
      // ignore parsing errors
    }
    return defaultViewerIceServers;
  }

  const urls = raw
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);

  if (urls.length === 0) {
    return defaultViewerIceServers;
  }

  return [{ urls }];
}

/** Map a peer-connection state to a UI status pill class. */
export function viewerStateClass(state: string) {
  switch (state) {
    case "connected":
      return "ok";
    case "failed":
    case "disconnected":
    case "closed":
      return "error";
    case "connecting":
    case "new":
      return "warn";
    default:
      return "muted";
  }
}

/** Human-readable label for a peer-connection state. */
export function viewerStateLabel(state: string) {
  switch (state) {
    case "connected":
      return "connecte";
    case "connecting":
      return "connexion";
    case "disconnected":
      return "deconnecte";
    case "failed":
      return "echec";
    case "closed":
      return "ferme";
    case "new":
      return "initialisation";
    default:
      return "attente";
  }
}

/** Map an inbound Mbps reading to a quality status pill class. */
export function viewerQualityClass(mbps: number | null) {
  if (mbps === null) return "muted";
  if (mbps < 0.2) return "error";
  if (mbps < 0.7) return "warn";
  return "ok";
}

/** Human-readable quality label keyed on inbound Mbps. */
export function viewerQualityLabel(mbps: number | null) {
  if (mbps === null) return "qualite en attente";
  if (mbps < 0.2) return "qualite faible";
  if (mbps < 0.7) return "qualite moyenne";
  if (mbps < 1.6) return "qualite bonne";
  return "qualite excellente";
}

/** Map an agent status string to a UI status pill class. */
export function statusClass(status: string | undefined) {
  switch ((status ?? "").toUpperCase()) {
    case "ONLINE":
      return "ok";
    case "BUSY":
      return "warn";
    default:
      return "muted";
  }
}

/** True when keyboard events should NOT be captured as remote input. */
export function isEditableTarget(target: EventTarget | null) {
  if (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement
  ) {
    return true;
  }

  return target instanceof HTMLElement && target.isContentEditable;
}
