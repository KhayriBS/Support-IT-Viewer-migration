import { onAgentUpdate, technicianApi } from "$lib/api";
import type { Agent, AppUser, ControlSession } from "$lib/api";

export interface DashboardStats {
  totalMachines: number;
  onlineMachines: number;
  offlineMachines: number;
  unassignedMachines: number;
  activeSessions: number;
  totalUsers: number;
}

// Singleton module-level state : un seul fetch sert toutes les pages
// (cartes, détails machines, détails utilisateurs). La navigation entre
// cartes est instantanée car la donnée est déjà là ; un refresh tourne
// en background pour la garder fraîche.
class DashboardDataStore {
  machines = $state<Agent[]>([]);
  users = $state<AppUser[]>([]);
  activeSessions = $state<ControlSession[]>([]);
  stats = $state<DashboardStats>({
    totalMachines: 0,
    onlineMachines: 0,
    offlineMachines: 0,
    unassignedMachines: 0,
    activeSessions: 0,
    totalUsers: 0
  });

  loadingMachines = $state(false);
  loadingUsers = $state(false);
  error = $state<string | null>(null);
  lastRefresh = $state<string>("");

  private started = false;
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private unsubRealtime: (() => void) | null = null;

  /** Démarre le préchargement + l'abonnement STOMP. Idempotent. */
  start = () => {
    if (this.started) return;
    this.started = true;

    void this.refreshAll();
    // Refresh régulier en filet de sécurité : STOMP fait le gros du travail
    this.pollTimer = setInterval(() => void this.refreshAll(), 30_000);
    this.unsubRealtime = onAgentUpdate((updated) => {
      const idx = this.machines.findIndex((m) => m.id === updated.id);
      if (idx >= 0) {
        this.machines = [
          ...this.machines.slice(0, idx),
          updated,
          ...this.machines.slice(idx + 1)
        ];
      } else {
        this.machines = [...this.machines, updated];
      }
      this.recomputeStats();
      this.lastRefresh = new Date().toLocaleTimeString();
    });
  };

  stop = () => {
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = null;
    this.unsubRealtime?.();
    this.unsubRealtime = null;
    this.started = false;
  };

  refreshAll = async () => {
    await Promise.all([this.refreshDashboard(), this.refreshUsers()]);
  };

  refreshDashboard = async () => {
    this.loadingMachines = true;
    try {
      const dash = await technicianApi.getAdminDashboard();
      if (dash) {
        this.machines = dash.machines ?? [];
        this.activeSessions = dash.activeSessions ?? [];
        this.stats = { ...this.stats, ...dash.stats };
        this.error = null;
        this.lastRefresh = new Date().toLocaleTimeString();
      }
    } catch (e) {
      this.error = String(e);
    } finally {
      this.loadingMachines = false;
    }
  };

  refreshUsers = async () => {
    this.loadingUsers = true;
    try {
      this.users = (await technicianApi.listUsers()) ?? [];
      this.stats = { ...this.stats, totalUsers: this.users.length };
      this.error = null;
    } catch (e) {
      this.error = String(e);
    } finally {
      this.loadingUsers = false;
    }
  };

  private recomputeStats() {
    const total = this.machines.length;
    const online = this.machines.filter((m) => m.status === "ONLINE").length;
    const unassigned = this.machines.filter(
      (m) => !m.assignedUsername || m.assignedUsername.trim() === ""
    ).length;
    this.stats = {
      ...this.stats,
      totalMachines: total,
      onlineMachines: online,
      offlineMachines: total - online,
      unassignedMachines: unassigned
    };
  }
}

export const dashboardData = new DashboardDataStore();
