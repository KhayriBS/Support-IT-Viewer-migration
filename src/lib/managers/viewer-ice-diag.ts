export type IceCandidateType =
  | "host"
  | "srflx"
  | "relay"
  | "prflx"
  | "unknown";

export function parseCandidateType(candidate: string): IceCandidateType {
  const m = candidate.match(/\stype\s(\S+)/i) ?? candidate.match(/\styp\s(\S+)/);
  const raw = m?.[1]?.toLowerCase() ?? "";
  if (raw === "host" || raw === "srflx" || raw === "relay" || raw === "prflx") {
    return raw;
  }
  return "unknown";
}

export function parseCandidateAddress(candidate: string): string | null {
  const parts = candidate.trim().split(/\s+/);
  if (parts.length < 6) return null;
  const ip = parts[4];
  return ip ?? null;
}

export function parseCandidatePort(candidate: string): number | null {
  const parts = candidate.trim().split(/\s+/);
  if (parts.length < 6) return null;
  const port = Number(parts[5]);
  return Number.isFinite(port) ? port : null;
}

export interface SelectedCandidatePair {
  pairType: IceCandidateType;
  local: { address: string | null; port: number | null; protocol: string | null };
  remote: { address: string | null; port: number | null; protocol: string | null };
}

interface CandidateLike {
  type?: string;
  candidateType?: string;
  address?: string;
  ip?: string;
  port?: number;
  protocol?: string;
}

interface CandidatePairLikeFull {
  type: string;
  nominated?: boolean;
  selected?: boolean;
  state?: string;
  localCandidateId?: string;
  remoteCandidateId?: string;
}

export async function readSelectedCandidatePair(
  pc: RTCPeerConnection
): Promise<SelectedCandidatePair | null> {
  const stats = await pc.getStats();
  const all = new Map<string, RTCStats>();
  const pairs: CandidatePairLikeFull[] = [];
  stats.forEach((s) => {
    all.set(s.id, s);
    if (s.type === "candidate-pair") {
      pairs.push(s as unknown as CandidatePairLikeFull);
    }
  });

  const activePair = pairs.find(
    (p) => (p.nominated && p.state === "succeeded") || p.selected === true,
  );
  if (!activePair) return null;

  const local = activePair.localCandidateId
    ? (all.get(activePair.localCandidateId) as CandidateLike | undefined)
    : undefined;
  const remote = activePair.remoteCandidateId
    ? (all.get(activePair.remoteCandidateId) as CandidateLike | undefined)
    : undefined;

  const localType = (local?.candidateType ?? local?.type ?? "unknown").toLowerCase();
  const remoteType = (remote?.candidateType ?? remote?.type ?? "unknown").toLowerCase();

  const pairType: IceCandidateType =
    localType === "relay" || remoteType === "relay"
      ? "relay"
      : localType === "srflx" || remoteType === "srflx"
        ? "srflx"
        : localType === "host" && remoteType === "host"
          ? "host"
          : localType === "prflx" || remoteType === "prflx"
            ? "prflx"
            : "unknown";

  return {
    pairType,
    local: {
      address: local?.address ?? local?.ip ?? null,
      port: local?.port ?? null,
      protocol: local?.protocol ?? null,
    },
    remote: {
      address: remote?.address ?? remote?.ip ?? null,
      port: remote?.port ?? null,
      protocol: remote?.protocol ?? null,
    },
  };
}

export function formatTransportSummary(pair: SelectedCandidatePair): string {
  const localAddr = pair.local.address ?? "?";
  const remoteAddr = pair.remote.address ?? "?";
  switch (pair.pairType) {
    case "host":
      return `LAN direct (${localAddr} ↔ ${remoteAddr})`;
    case "srflx":
      return `P2P via STUN (${localAddr} ↔ ${remoteAddr})`;
    case "relay":
      return `Via TURN relay (${localAddr} ↔ ${remoteAddr})`;
    case "prflx":
      return `Peer-reflexive (${localAddr} ↔ ${remoteAddr})`;
    default:
      return `Connexion établie (${localAddr} ↔ ${remoteAddr})`;
  }
}
