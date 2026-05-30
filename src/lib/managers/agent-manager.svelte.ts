import { invoke } from "@tauri-apps/api/core";
import { technicianApi } from "$lib/api";

export interface AgentMetrics {
  cpuUsage: number;
  ramUsage: number;
  diskUsage: number;
  timestamp: number;
}

interface AgentStatusSnapshot {
  running: boolean;
  machineId: string;
  role?: string;
  assignedUsername?: string | null;
}

export type AgentRole = "TECHNICIAN" | "USER" | "PENDING" | "";

class AgentManager {
  metrics = $state<AgentMetrics | null>(null);
  metricsError = $state<string | null>(null);
  metricsLoading = $state(true);
  metricsPanelOpen = $state(false);
  agentRunning = $state(false);
  agentLifecycleError = $state<string | null>(null);

  localMachineId = $state<string>("");
  localConnectionCode = $state<string>("");
  localConnectionCodeLoading = $state(false);
  localConnectionCodeError = $state<string | null>(null);
  connectionCodeCopied = $state(false);

  role = $state<AgentRole>("");
  assignedUsername = $state<string | null>(null);

  refreshMetrics = async () => {
    try {
      const payload = await invoke<AgentMetrics>("get_metrics");
      this.metrics = payload;
      this.metricsError = null;
    } catch (error) {
      this.metricsError = String(error);
    } finally {
      this.metricsLoading = false;
    }
  };

  syncLifecycle = async () => {
    try {
      let status = await invoke<AgentStatusSnapshot>("get_agent_status");
      this.agentRunning = status.running;

      if (!status.running) {
        await invoke("start_agent_cmd", { serverUrl: technicianApi.baseUrl });
        status = await invoke<AgentStatusSnapshot>("get_agent_status");
        this.agentRunning = status.running;
      }

      this.localMachineId = status.machineId?.trim() ?? "";
      this.applyAuthSnapshot(status);
      await this.persistTokenInLocalStorage();
      await this.refreshLocalConnectionCode();

      this.agentLifecycleError = null;
    } catch (error) {
      this.agentLifecycleError = String(error);
      this.agentRunning = false;
    }
  };

  /** Re-call /agents/login (via Tauri) for PENDING polling. */
  refreshRole = async (): Promise<AgentRole> => {
    try {
      const newRole = await invoke<string>("refresh_agent_role_cmd");
      const status = await invoke<AgentStatusSnapshot>("get_agent_status");
      this.applyAuthSnapshot(status);
      await this.persistTokenInLocalStorage();
      return (newRole as AgentRole) || this.role;
    } catch (error) {
      this.agentLifecycleError = String(error);
      return this.role;
    }
  };

  private applyAuthSnapshot(status: AgentStatusSnapshot) {
    const incoming = (status.role ?? "").trim();
    if (incoming === "TECHNICIAN" || incoming === "USER" || incoming === "PENDING") {
      this.role = incoming;
    } else if (!incoming) {
      this.role = "";
    }
    this.assignedUsername = status.assignedUsername?.trim?.() || null;
  }

  // Le JWT agent vit dans Rust ; on le miroite dans localStorage pour que
  // `technicianApi` (qui lit getStoredToken()) attache automatiquement le
  // header Authorization sur tous les appels REST. Authentifie aussi les
  // calls /admin/** (le JWT porte l'authority owner ROLE_ADMIN).
  private persistTokenInLocalStorage = async () => {
    if (typeof localStorage === "undefined") return;
    try {
      const token = await invoke<string>("get_agent_token");
      if (token) {
        localStorage.setItem("token", token);
      } else {
        localStorage.removeItem("token");
      }
    } catch {
      /* tolérer un échec ponctuel — on retentera au prochain sync */
    }
  };

  stopLifecycle = async () => {
    try {
      await invoke("stop_agent_cmd");
    } catch {
      // ignore shutdown errors
    } finally {
      this.agentRunning = false;
    }
  };

  refreshLocalConnectionCode = async () => {
    const machineId = this.localMachineId.trim();
    if (!machineId) {
      this.localConnectionCode = "";
      this.localConnectionCodeError = null;
      return;
    }

    this.localConnectionCodeLoading = true;
    try {
      const response = await technicianApi.getMachineAuthStatus(machineId);
      this.localConnectionCode = response?.data?.connectionCode?.trim?.() ?? "";
      this.localConnectionCodeError = null;
    } catch (error) {
      this.localConnectionCode = "";
      this.localConnectionCodeError = String(error);
    } finally {
      this.localConnectionCodeLoading = false;
    }
  };

  copyConnectionCode = async () => {
    if (!this.localConnectionCode) {
      return;
    }

    try {
      await navigator.clipboard.writeText(this.localConnectionCode);
      this.connectionCodeCopied = true;
      setTimeout(() => {
        this.connectionCodeCopied = false;
      }, 1600);
    } catch {
      this.connectionCodeCopied = false;
    }
  };

  loadMachineId = async () => {
    try {
      const status = await invoke<AgentStatusSnapshot>("get_agent_status");
      this.localMachineId = status.machineId?.trim() ?? "";
    } catch {
      this.localMachineId = "";
    }
  };

  toggleMetricsPanel = () => {
    this.metricsPanelOpen = !this.metricsPanelOpen;
  };
}

export const agentManager = new AgentManager();
