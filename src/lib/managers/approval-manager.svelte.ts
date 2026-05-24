import { technicianApi } from "$lib/api";
import type { ControlSession } from "$lib/api";
import { agentManager } from "./agent-manager.svelte";

export interface ApprovalDecision {
  session: ControlSession;
  allowRemoteInput: boolean;
  allowFileTransfer: boolean;
}

class ApprovalManager {
  open = $state(false);
  pendingSession = $state<ControlSession | null>(null);
  allowRemoteInput = $state(true);
  allowFileTransfer = $state(true);
  loading = $state(false);
  error = $state<string | null>(null);

  /**
   * Callback invoked when the user approves a pending session.
   * The parent uses this to promote the approved session to active.
   */
  onApproved: ((decision: ApprovalDecision) => void) | null = null;

  /** Poll the backend for a pending approval targeting this machine. */
  check = async () => {
    if (!agentManager.localMachineId) {
      await agentManager.loadMachineId();
      if (!agentManager.localMachineId) return;
    }

    try {
      const session = await technicianApi.getPendingApprovalPublic(agentManager.localMachineId);
      if (session && session.status === "PENDING_APPROVAL") {
        this.pendingSession = session;
        this.open = true;
        this.allowRemoteInput = true;
        this.allowFileTransfer = true;
      }
      this.error = null;
    } catch {
      // Silent fail for polling
    }
  };

  approve = async () => {
    if (!this.pendingSession || this.loading) return;

    this.loading = true;
    this.error = null;

    try {
      await technicianApi.approveSessionPublic(
        this.pendingSession.id,
        this.allowRemoteInput,
        this.allowFileTransfer
      );

      const decision: ApprovalDecision = {
        session: this.pendingSession,
        allowRemoteInput: this.allowRemoteInput,
        allowFileTransfer: this.allowFileTransfer
      };

      this.loading = false;
      this.open = false;
      this.pendingSession = null;

      this.onApproved?.(decision);
    } catch (error) {
      this.loading = false;
      this.error = String(error);
    }
  };

  reject = async () => {
    if (!this.pendingSession || this.loading) return;

    this.loading = true;
    this.error = null;

    try {
      await technicianApi.rejectSessionPublic(this.pendingSession.id);
      this.loading = false;
      this.open = false;
      this.pendingSession = null;
    } catch (error) {
      this.loading = false;
      this.error = String(error);
    }
  };

  close = () => {
    this.open = false;
  };
}

export const approvalManager = new ApprovalManager();
