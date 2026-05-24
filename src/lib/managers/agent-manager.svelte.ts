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
}

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
      await this.refreshLocalConnectionCode();

      this.agentLifecycleError = null;
    } catch (error) {
      this.agentLifecycleError = String(error);
      this.agentRunning = false;
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
