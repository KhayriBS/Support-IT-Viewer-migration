export interface ApiResponse<T> {
  success: boolean;
  data: T;
  message: string | null;
}

export interface Agent {
  id: number;
  machineId: string;
  hostname: string;
  osInfo: string;
  ipAddress: string;
  status: string;
  lastHeartbeat: string;
  assignedUsername?: string;
  connectionCode?: string;
  cpuUsage?: number;
  memoryUsage?: number;
  diskUsage?: number;
}

export interface AgentMetrics {
  cpuUsage: number;
  ramUsage: number;
  diskUsage: number;
  timestamp: number;
}

export interface ControlSession {
  id: number;
  signalingToken: string;
  agentMachineId: string;
  technicianUsername: string;
  technicianRole?: string;
  allowRemoteInput?: boolean;
  allowFileTransfer?: boolean;
  status: "PENDING_APPROVAL" | "ACTIVE" | "TERMINATED";
  startedAt: string;
  endedAt: string | null;
}

export interface SessionHistoryEntry {
  id: number;
  agentMachineId: string;
  technicianUsername: string;
  technicianRole?: string | null;
  status: "PENDING_APPROVAL" | "ACTIVE" | "TERMINATED";
  /** "incoming" si la machine demandée est l'agent, "outgoing" si elle est le technicien */
  direction: "incoming" | "outgoing";
  /** Identifiant de l'AUTRE peer à afficher (technicien si entrante, agent si sortante) */
  peerLabel: string;
  startedAt: string;
  endedAt: string | null;
  durationMs: number | null;
}

export interface LoginRequest {
  email: string;
  password: string;
  machineId?: string;
}

export interface RegisterRequest {
  fullName: string;
  email: string;
  password: string;
  phoneNumber: string;
  department: string;
  machineId?: string;
}

export interface MachineAuthStatus {
  machineExists: boolean;
  hasAssignedUser: boolean;
  assignedUsername: string | null;
  connectionCode: string | null;
}

export interface ChatMessage {
  id?: number;
  roomId: string;
  senderRole: string;
  senderName: string;
  receiverRole?: string;
  receiverName?: string;
  content: string;
  timestamp: string;
  delivered?: boolean;
}

export interface TypingNotification {
  senderName: string;
  senderRole: string;
  isTyping: boolean;
}

// ─── File transfer types ──────────────────────────────────────────────────────

/**
 * Sens du transfert tel que stocké en base, du point de vue technicien :
 *  - UPLOAD   : technicien (viewer) envoie au PC distant (agent)
 *  - DOWNLOAD : technicien rapatrie un fichier depuis le PC distant
 */
export type FileTransferDirection = "UPLOAD" | "DOWNLOAD";
export type FileTransferStatus = "IN_PROGRESS" | "COMPLETED" | "FAILED" | "CANCELLED";

export interface FileTransferStartRequest {
  transferId: string;
  sessionId?: number | null;
  fromMachineId: string;
  toMachineId: string;
  direction: FileTransferDirection;
  fileName: string;
  fileSize?: number | null;
  mimeType?: string | null;
  destPath?: string | null;
}

export interface FileTransferUpdateRequest {
  status: FileTransferStatus;
  fileSize?: number | null;
  errorMessage?: string | null;
  destPath?: string | null;
}

export interface FileTransferLogEntry {
  id: number;
  transferId: string;
  sessionId: number | null;
  fromMachineId: string;
  toMachineId: string;
  direction: FileTransferDirection;
  fileName: string;
  fileSize: number;
  mimeType: string | null;
  destPath: string | null;
  status: FileTransferStatus;
  errorMessage: string | null;
  startedAt: string;
  completedAt: string | null;
}

export interface FileTransferHistoryEntry extends FileTransferLogEntry {
  /** "incoming" si la machine demandée est destinataire, "outgoing" si elle est expéditrice */
  listDirection: "incoming" | "outgoing";
  /** Identifiant de l'AUTRE peer (à afficher dans la carte) */
  peerLabel: string;
  durationMs: number | null;
}

export interface FileEntry {
  name: string;
  path: string;
  isDirectory: boolean;
  size: number;
  lastModified: number;
}

export interface RemoteFileList {
  path: string;
  files: FileEntry[];
  error?: string | null;
}

export type FileTransferState = "active" | "complete" | "error";

export interface FileTransfer {
  transferId: string;
  type: "upload" | "download";
  fileName: string;
  totalSize: number;
  totalChunks: number;
  doneChunks: number;
  doneBytes: number;
  startedAt: number;
  state: FileTransferState;
  error?: string;
  /** Chemin absolu où l'agent a sauvegardé le fichier (renvoyé via FILE_UPLOAD_ACK) */
  destPath?: string;
  /** accumulated ArrayBuffers while download is in progress */
  buffers?: ArrayBuffer[];
}

// ─────────────────────────────────────────────────────────────────────────────

export type SignalType =
  | "JOIN"
  | "OFFER"
  | "ANSWER"
  | "ICE"
  | "LEAVE"
  | "CHAT"
  | "STREAM_STATS"
  | "STREAM_PROFILE"
  | "ERROR"
  | "FILE_LIST_REQUEST"
  | "FILE_LIST"
  | "FILE_DOWNLOAD_REQUEST"
  | "FILE_UPLOAD_REQUEST"
  | "FILE_DATA"
  | "FILE_COMPLETE"
  | "FILE_ERROR";

export interface SignalMessage {
  type: SignalType;
  from?: string;
  to: string;
  sessionId?: string;
  payload: unknown;
}
