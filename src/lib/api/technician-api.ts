import type {
  Agent,
  AgentMetrics,
  ApiResponse,
  ChatMessage,
  ControlSession,
  FileTransferHistoryEntry,
  FileTransferLogEntry,
  FileTransferStartRequest,
  FileTransferUpdateRequest,
  LoginRequest,
  MachineAuthStatus,
  RegisterRequest,
  SessionHistoryEntry
} from "$lib/api/types";

const API_URL = (import.meta.env.VITE_API_URL as string | undefined)?.replace(/\/$/, "")
  ?? "https://signaling-server-tgsj.onrender.com";

type HttpMethod = "GET" | "POST" | "PATCH" | "PUT" | "DELETE";

interface RequestOptions {
  method?: HttpMethod;
  token?: string;
  body?: unknown;
}

function getStoredToken(): string | undefined {
  if (typeof localStorage === "undefined") {
    return undefined;
  }

  const token = localStorage.getItem("token")?.trim();
  return token ? token : undefined;
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = "GET", token, body } = options;
  const authToken = token ?? getStoredToken();

  let headers: HeadersInit = {
    "Content-Type": "application/json"
  };

  if (authToken) {
    headers = {
      ...headers,
      Authorization: `Bearer ${authToken}`
    };
  }

  const response = await fetch(`${API_URL}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body)
  });

  if (response.status === 204 || response.status === 404) {
    return null as T;
  }

  if (!response.ok) {
    const text = await response.text().catch(() => "");
    throw new Error(`HTTP ${response.status} ${path}${text ? `: ${text}` : ""}`);
  }

  return (await response.json()) as T;
}

function unwrap<T>(res: ApiResponse<T>): T {
  return res.data;
}

export const technicianApi = {
  baseUrl: API_URL,

  // AgentService migration
  getAllAgents(token?: string) {
    return request<Agent[]>("/agents", { token });
  },

  getOnlineAgents(token?: string) {
    return request<Agent[]>("/agents/online", { token });
  },

  async startSession(machineId: string, token?: string) {
    const res = await request<ApiResponse<ControlSession>>(`/sessions/start/${machineId}`, {
      method: "POST",
      token,
      body: {}
    });
    return unwrap(res);
  },

  /**
   * Historique des sessions impliquant une machine (côté agent ou technicien).
   * Voir SessionController#getSessionHistory côté backend.
   */
  async getSessionHistory(
    machineId: string,
    options: { direction?: "incoming" | "outgoing" | "all"; status?: string; q?: string } = {},
    token?: string
  ) {
    const params = new URLSearchParams();
    if (options.direction && options.direction !== "all") params.set("direction", options.direction);
    if (options.status && options.status !== "all") params.set("status", options.status);
    if (options.q && options.q.trim()) params.set("q", options.q.trim());
    const qs = params.toString();
    const path = `/sessions/history/${encodeURIComponent(machineId)}${qs ? `?${qs}` : ""}`;
    const res = await request<ApiResponse<SessionHistoryEntry[]>>(path, { token });
    return unwrap(res) ?? [];
  },

  async startSessionByCode(code: string, token?: string) {
    const res = await request<ApiResponse<ControlSession>>(`/sessions/start-by-code/${code}`, {
      method: "POST",
      token,
      body: {}
    });
    return unwrap(res);
  },

  async stopSession(sessionId: number, token?: string) {
    await request<ApiResponse<void>>(`/sessions/stop/${sessionId}`, {
      method: "POST",
      token,
      body: {}
    });
  },

  async stopSessionByToken(sessionToken: string, token?: string) {
    await request<ApiResponse<void>>(`/sessions/stop-by-token/${sessionToken}`, {
      method: "POST",
      token,
      body: {}
    });
  },

  getPendingApproval(machineId: string, token?: string) {
    return request<ControlSession | null>(`/sessions/approval/${machineId}`, { token });
  },

  getPendingApprovalPublic(machineId: string, token?: string) {
    return request<ControlSession | null>(`/sessions/approval-public/${machineId}`, { token });
  },

  async approveSession(sessionId: number, allowRemoteInput: boolean, allowFileTransfer: boolean, token?: string) {
    await request<ApiResponse<void>>(`/sessions/approve/${sessionId}`, {
      method: "POST",
      token,
      body: { allowRemoteInput, allowFileTransfer }
    });
  },

  async rejectSession(sessionId: number, token?: string) {
    await request<ApiResponse<void>>(`/sessions/reject/${sessionId}`, {
      method: "POST",
      token,
      body: {}
    });
  },

  async approveSessionPublic(sessionId: number, allowRemoteInput: boolean, allowFileTransfer: boolean, token?: string) {
    await request<ApiResponse<void>>(`/sessions/approve-public/${sessionId}`, {
      method: "POST",
      token,
      body: { allowRemoteInput, allowFileTransfer }
    });
  },

  async rejectSessionPublic(sessionId: number, token?: string) {
    await request<ApiResponse<void>>(`/sessions/reject-public/${sessionId}`, {
      method: "POST",
      token,
      body: {}
    });
  },

  getSessionByToken(sessionToken: string, token?: string) {
    return request<ControlSession | null>(`/sessions/by-token/${sessionToken}`, { token });
  },

  getMetricsHistory(machineId: string, token?: string) {
    return request<AgentMetrics[]>(`/agents/metrics/${machineId}`, { token });
  },

  assignAgent(agentId: number, username: string, token?: string) {
    return request<Agent>(`/agents/${agentId}/assign/${encodeURIComponent(username)}`, {
      method: "POST",
      token,
      body: {}
    });
  },

  unassignAgent(agentId: number, token?: string) {
    return request<Agent>(`/agents/${agentId}/unassign`, {
      method: "POST",
      token,
      body: {}
    });
  },

  // AuthService migration
  login(payload: LoginRequest) {
    return request<ApiResponse<string>>("/auth/login", {
      method: "POST",
      body: payload
    });
  },

  register(payload: RegisterRequest) {
    return request<ApiResponse<string>>("/auth/register", {
      method: "POST",
      body: payload
    });
  },

  getMachineAuthStatus(machineId: string, token?: string) {
    return request<ApiResponse<MachineAuthStatus>>(`/auth/machine-status/${machineId}`, { token });
  },

  // ChatService migration (REST fallback endpoints)
  async sendMessageRest(
    roomId: string,
    senderRole: string,
    senderName: string,
    receiverRole: string,
    receiverName: string,
    content: string,
    token?: string
  ) {
    await request<ApiResponse<void>>(`/chat/send/${encodeURIComponent(roomId)}`, {
      method: "POST",
      token,
      body: { senderRole, senderName, receiverRole, receiverName, content }
    });
  },

  async getMessages(roomId: string, token?: string) {
    const res = await request<ApiResponse<ChatMessage[]>>(`/chat/messages/${encodeURIComponent(roomId)}`, { token });
    return res.data ?? [];
  },

  async getPendingMessages(roomId: string, token?: string) {
    const res = await request<ApiResponse<ChatMessage[]>>(`/chat/pending/${encodeURIComponent(roomId)}`, { token });
    return res.data ?? [];
  },

  // ── File transfer logging (audit trail dans la BD) ────────────────────────

  /**
   * Enregistre le début d'un transfert P2P en BD. Idempotent sur transferId :
   * si la même UUID est rejouée, le serveur met à jour la ligne existante.
   * À appeler dès que le client choisit un fichier à envoyer / reçoit un
   * FILE_DOWNLOAD_RESPONSE.
   */
  async logFileTransferStart(payload: FileTransferStartRequest, token?: string) {
    const res = await request<ApiResponse<FileTransferLogEntry>>("/file-transfers", {
      method: "POST",
      token,
      body: payload
    });
    return unwrap(res);
  },

  /**
   * Met à jour le statut d'un transfert (COMPLETED / FAILED / CANCELLED).
   * À appeler à la fin pour finaliser la ligne de log avec completedAt.
   */
  async logFileTransferUpdate(transferId: string, payload: FileTransferUpdateRequest, token?: string) {
    const res = await request<ApiResponse<FileTransferLogEntry>>(
      `/file-transfers/${encodeURIComponent(transferId)}`,
      { method: "PATCH", token, body: payload }
    );
    return unwrap(res);
  },

  /**
   * Historique des transferts d'une machine donnée (machineId direct ou
   * connection_code 6 chiffres). Filtres direction/status/q (search).
   */
  async getFileTransferHistory(
    machineId: string,
    options: {
      direction?: "incoming" | "outgoing" | "all";
      status?: "in_progress" | "completed" | "failed" | "cancelled" | "ended" | "all" | string;
      q?: string;
    } = {},
    token?: string
  ) {
    const params = new URLSearchParams();
    if (options.direction && options.direction !== "all") params.set("direction", options.direction);
    if (options.status && options.status !== "all") params.set("status", options.status);
    if (options.q && options.q.trim()) params.set("q", options.q.trim());
    const qs = params.toString();
    const path = `/file-transfers/history/${encodeURIComponent(machineId)}${qs ? `?${qs}` : ""}`;
    const res = await request<ApiResponse<FileTransferHistoryEntry[]>>(path, { token });
    return unwrap(res) ?? [];
  }
};
