import type {
  Agent,
  AgentMetrics,
  ApiResponse,
  ChatMessage,
  ControlSession,
  LoginRequest,
  MachineAuthStatus,
  RegisterRequest
} from "$lib/api/types";

const API_URL = (import.meta.env.VITE_API_URL as string | undefined)?.replace(/\/$/, "")
  ?? "http://196.187.133.6:8080";

type HttpMethod = "GET" | "POST";

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
  }
};
