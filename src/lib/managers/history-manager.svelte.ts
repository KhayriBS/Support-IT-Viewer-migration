import { technicianApi } from "$lib/api";
import type { SessionHistoryEntry, FileTransferHistoryEntry } from "$lib/api/types";
import type { RdSessionTypeFilter, RdSessionStatusFilter, RdFileFilter } from "$lib/types/ui";
import { agentManager } from "./agent-manager.svelte";

class HistoryManager {
  sessions = $state<SessionHistoryEntry[]>([]);
  sessionsLoading = $state(false);
  sessionsError = $state<string | null>(null);
  sessionSearch = $state("");
  sessionTypeFilter = $state<RdSessionTypeFilter>("all");
  sessionStatusFilter = $state<RdSessionStatusFilter>("all");

  files = $state<FileTransferHistoryEntry[]>([]);
  filesLoading = $state(false);
  filesError = $state<string | null>(null);
  fileSearch = $state("");
  fileFilter = $state<RdFileFilter>("all");

  private sessionsTimer: ReturnType<typeof setTimeout> | null = null;
  private filesTimer: ReturnType<typeof setTimeout> | null = null;

  /** Prefer connection_code (what the backend table stores natively), fall back to machineId. */
  private resolveKey(): string {
    return (agentManager.localConnectionCode || agentManager.localMachineId || "").trim();
  }

  fetchSessions = async () => {
    const key = this.resolveKey();
    if (!key) {
      this.sessions = [];
      return;
    }
    this.sessionsLoading = true;
    this.sessionsError = null;
    try {
      this.sessions = await technicianApi.getSessionHistory(key, {
        direction: this.sessionTypeFilter,
        status: this.sessionStatusFilter,
        q: this.sessionSearch
      });
    } catch (err) {
      this.sessionsError = String(err);
      this.sessions = [];
    } finally {
      this.sessionsLoading = false;
    }
  };

  fetchFiles = async () => {
    const key = this.resolveKey();
    if (!key) {
      this.files = [];
      return;
    }
    this.filesLoading = true;
    this.filesError = null;
    try {
      // fileFilter "upload" = sortant (ce PC envoie) → "outgoing"
      // fileFilter "download" = entrant (ce PC reçoit) → "incoming"
      const direction =
        this.fileFilter === "upload" ? "outgoing"
          : this.fileFilter === "download" ? "incoming"
          : "all";
      this.files = await technicianApi.getFileTransferHistory(key, {
        direction,
        status: "all",
        q: this.fileSearch
      });
    } catch (err) {
      this.filesError = String(err);
      this.files = [];
    } finally {
      this.filesLoading = false;
    }
  };

  /** Debounced session refresh — call from $effect, won't spam the backend while user types. */
  scheduleSessionsRefresh = (delayMs = 250) => {
    if (this.sessionsTimer) clearTimeout(this.sessionsTimer);
    this.sessionsTimer = setTimeout(() => { void this.fetchSessions(); }, delayMs);
  };

  scheduleFilesRefresh = (delayMs = 250) => {
    if (this.filesTimer) clearTimeout(this.filesTimer);
    this.filesTimer = setTimeout(() => { void this.fetchFiles(); }, delayMs);
  };
}

export const historyManager = new HistoryManager();
