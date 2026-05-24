import { technicianApi } from "$lib/api";
import type { ControlSession } from "$lib/api";
import { signalBus } from "./signal-bus.svelte";
import { chatManager } from "./chat-manager.svelte";

class SessionManager {
  activeSession = $state<ControlSession | null>(null);
  queriedSession = $state<ControlSession | null>(null);
  selectedFeature = $state<"screen" | "chat" | "files" | null>(null);
  actionLoading = $state(false);
  actionError = $state<string | null>(null);
  waitingForApproval = $state(false);
  sessionTokenQuery = $state("");

  private sessionActivationTimer: ReturnType<typeof setInterval> | null = null;

  // ── Callbacks set by the orchestrator (+page.svelte) ─────────────────────
  /** Open the signaling WebSocket once a session reaches ACTIVE. */
  connectSignaling: () => Promise<void> = async () => {};
  /** Close the signaling WebSocket and send LEAVE. */
  disconnectSignaling: (opts?: { sendLeave?: boolean }) => Promise<void> = async () => {};
  /** Read the current user-entered connection code (for startSessionWithCode). */
  getConnectionCode: () => string = () => "";

  // ── Internal helpers ─────────────────────────────────────────────────────
  private stopActivationWatch = () => {
    if (this.sessionActivationTimer) {
      clearInterval(this.sessionActivationTimer);
      this.sessionActivationTimer = null;
    }
  };

  private watchActivation = (sessionToken: string) => {
    this.stopActivationWatch();
    if (!sessionToken) return;

    let attempts = 0;
    let inFlight = false;

    this.sessionActivationTimer = setInterval(async () => {
      if (inFlight || signalBus.signalingConnected) return;

      inFlight = true;
      attempts += 1;

      try {
        const session = await technicianApi.getSessionByToken(sessionToken);
        if (!session) {
          if (attempts >= 90) this.stopActivationWatch();
          return;
        }

        this.queriedSession = session;
        this.activeSession = session;

        if (session.status === "ACTIVE") {
          this.waitingForApproval = false;
          // No auto-screen — user picks Écran/Fichier/Chat from the menu.
          this.stopActivationWatch();
          await this.connectSignaling();
          return;
        }

        if (session.status === "TERMINATED" || attempts >= 90) {
          this.waitingForApproval = false;
          this.stopActivationWatch();
        }
      } catch {
        if (attempts >= 90) {
          this.waitingForApproval = false;
          this.stopActivationWatch();
        }
      } finally {
        inFlight = false;
      }
    }, 2000);
  };

  // ── Public API ───────────────────────────────────────────────────────────
  startSession = async (machineId: string) => {
    this.actionLoading = true;
    this.actionError = null;
    this.waitingForApproval = false;
    this.selectedFeature = null;
    try {
      this.activeSession = await technicianApi.startSession(machineId);
      this.queriedSession = this.activeSession;
      this.sessionTokenQuery = this.activeSession.signalingToken;
      this.waitingForApproval = this.activeSession.status === "PENDING_APPROVAL";
      this.watchActivation(this.activeSession.signalingToken);
    } catch (error) {
      this.actionError = String(error);
      this.waitingForApproval = false;
    } finally {
      this.actionLoading = false;
    }
  };

  startSessionWithCode = async () => {
    const code = this.getConnectionCode().trim();
    if (!code) {
      this.actionError = "Veuillez renseigner un code de connexion.";
      return;
    }

    this.actionLoading = true;
    this.actionError = null;
    this.waitingForApproval = false;
    this.selectedFeature = null;
    try {
      this.activeSession = await technicianApi.startSessionByCode(code);
      this.queriedSession = this.activeSession;
      this.sessionTokenQuery = this.activeSession.signalingToken;
      this.waitingForApproval = this.activeSession.status === "PENDING_APPROVAL";
      this.watchActivation(this.activeSession.signalingToken);
    } catch (error) {
      this.actionError = String(error);
      this.waitingForApproval = false;
    } finally {
      this.actionLoading = false;
    }
  };

  stopByToken = async () => {
    const token = (this.activeSession?.signalingToken ?? this.sessionTokenQuery).trim();
    if (!token) {
      this.actionError = "Aucun token de session a arreter.";
      return;
    }

    this.actionLoading = true;
    this.actionError = null;
    try {
      this.stopActivationWatch();
      await this.disconnectSignaling({ sendLeave: true });
      chatManager.disconnect();
      await technicianApi.stopSessionByToken(token);
      this.activeSession = null;
      this.queriedSession = null;
      this.waitingForApproval = false;
      this.selectedFeature = null;
    } catch (error) {
      this.actionError = String(error);
    } finally {
      this.actionLoading = false;
    }
  };

  chooseFeature = (feature: "screen" | "chat" | "files") => {
    this.selectedFeature = feature;
    if (feature === "chat") {
      void chatManager.connect();
    }
  };

  lookupSession = async () => {
    const token = this.sessionTokenQuery.trim();
    if (!token) {
      this.actionError = "Veuillez renseigner un token de session.";
      return;
    }

    this.actionLoading = true;
    this.actionError = null;
    try {
      this.queriedSession = await technicianApi.getSessionByToken(token);
    } catch (error) {
      this.actionError = String(error);
    } finally {
      this.actionLoading = false;
    }
  };
}

export const sessionManager = new SessionManager();
