/**
 * Row shape used by RdFileHistory — merges backend audit entries with the
 * live in-flight transfers that don't yet have a persisted record.
 */
export type RdFileRow = {
  transferId: string;
  fileName: string;
  /** "upload" = ce PC a envoyé, "download" = ce PC a reçu */
  type: "upload" | "download";
  /** Identifiant lisible de l'autre PC */
  peerLabel: string;
  sizeBytes: number;
  state: "active" | "complete" | "error" | "cancelled";
  error: string | null;
  /** Epoch ms pour le tri */
  startedMs: number;
  /** Pour les transferts en cours : progression */
  doneBytes: number;
  isLive: boolean;
};

export type RdSessionTypeFilter = "all" | "incoming" | "outgoing";
export type RdSessionStatusFilter = "all" | "active" | "ended";
export type RdFileFilter = "all" | "upload" | "download";
