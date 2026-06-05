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
  /**
   * Côté CIBLE uniquement : true si l'utilisateur a cliqué "← Retour" pour
   * masquer la vue session sans la terminer. Le +layout ne redirige plus vers
   * /dashboard tant que ce flag est true. /my-machines affiche alors une
   * bannière "Reprendre la session" qui le remet à false → re-redirection.
   * Reset automatiquement à false quand la session change.
   */
  dismissedByAgent = $state(false);

  private sessionActivationTimer: ReturnType<typeof setInterval> | null = null;
  private sessionTerminationTimer: ReturnType<typeof setInterval> | null = null;

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

  private stopTerminationWatch = () => {
    if (this.sessionTerminationTimer) {
      clearInterval(this.sessionTerminationTimer);
      this.sessionTerminationTimer = null;
    }
  };

  /**
   * Reset complet du state session — utilisé quand le serveur a marqué la
   * session TERMINATED (le tech a fait Déconnecter, ou timeout).
   */
  clearSessionLocal = () => {
    this.stopTerminationWatch();
    this.stopActivationWatch();
    this.activeSession = null;
    this.queriedSession = null;
    this.selectedFeature = null;
    this.waitingForApproval = false;
    this.sessionTokenQuery = "";
    this.dismissedByAgent = false;
  };

  /**
   * Côté CIBLE : masque la vue session sans la terminer côté serveur.
   * Le Rust agent continue de streamer, le technicien ne voit aucune
   * coupure. Le +layout ne redirige plus vers /dashboard. L'agent peut
   * revenir via le bouton "Reprendre la session" sur /my-machines.
   */
  dismissSessionLocally = () => {
    this.dismissedByAgent = true;
    this.selectedFeature = null;
  };

  /** Annule le masquage et permet à +layout de re-rediriger vers /dashboard. */
  resumeSessionView = () => {
    this.dismissedByAgent = false;
  };

  /**
   * Poll le statut de la session toutes les 4 s. Dès qu'elle passe TERMINATED
   * (ou disparaît), clear local + le router +layout renvoie automatiquement
   * sur la route du rôle (USER → /my-machines, TECHNICIAN → /dashboard accueil).
   */
  watchTermination = (sessionToken: string) => {
    this.stopTerminationWatch();
    if (!sessionToken) return;

    this.sessionTerminationTimer = setInterval(async () => {
      try {
        const session = await technicianApi.getSessionByToken(sessionToken);
        if (!session || session.status === "TERMINATED") {
          this.clearSessionLocal();
        }
      } catch {
        /* best-effort, on retentera au prochain tick */
      }
    }, 4000);
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
      this.watchTermination(this.activeSession.signalingToken);
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
      this.watchTermination(this.activeSession.signalingToken);
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
      this.stopTerminationWatch();
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
