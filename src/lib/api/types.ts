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

export type SignalType =
  | "JOIN"
  | "OFFER"
  | "ANSWER"
  | "ICE"
  | "LEAVE"
  | "CHAT"
  | "STREAM_STATS"
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
