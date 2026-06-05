import { AiRealtimeClient } from "$lib/api";
import type { AiAction, AiActionEnvelope, AiHistoryStep, ChatMessage, ControlSession } from "$lib/api";
import { formatBytesApprox } from "$lib/utils/format";
import { viewerPeer } from "./viewer-peer.svelte";

interface AiActionResult {
  type: "AI_ACTION_RESULT";
  ok: boolean;
  message?: string;
  /** Base64 JPEG sans prefixe data: — present uniquement pour action="screenshot". */
  screenshot?: string;
  /** Type de l'action executee (click, type_text, shell, screenshot, …). */
  action?: string;
}

/** Demande d'approbation shell émise par l'agent Rust quand une commande
 *  est hors allow-list. Le viewer affiche un modal de validation. */
export interface PendingShellApproval {
  actionId: string;
  cmd: string;
  shell: string;
  /** Timestamp d'arrivée — utilisé pour timeout côté UI (60s = même
   *  fenêtre que côté Rust). */
  receivedAt: number;
}

interface PendingScreenshot {
  resolve: (payload: { jpegBase64: string; width: number; height: number }) => void;
  reject: (err: Error) => void;
  timeoutId: ReturnType<typeof setTimeout>;
  expectedChunks?: number;
  receivedChunks?: string[];
  width?: number;
  height?: number;
}

const AI_MIN_INTERVAL_MS = 6_000;
const AI_BUSY_WATCHDOG_MS = 60_000;

// ── Boucle agentic ──────────────────────────────────────────────────────────
/** Plafond dur d'itérations pour une même commande utilisateur. Au-delà,
 *  on stop même si Gemini renvoie done=false (protection contre boucle
 *  infinie sur un Gemini buggé ou un objectif impossible à atteindre). */
const AGENTIC_MAX_ITERATIONS = 5;
/** Délai entre la fin d'exécution des actions et la relance auto du tour
 *  suivant — laisse le temps à l'UI distante de se stabiliser (animations,
 *  ouverture de fenêtres) avant la prochaine capture. */
const AGENTIC_INTER_TURN_DELAY_MS = 800;
/** Cap sur le nombre de caractères de stdout/résultat conservé dans le
 *  resultText envoyé à Gemini au tour suivant. Trop grand = budget tokens
 *  exploré, trop petit = perte de signal utile (ex. sortie ipconfig). */
const AGENTIC_RESULT_TEXT_MAX = 600;

class AiPipeline {
  client = new AiRealtimeClient();

  aiConnected = $state(false);
  aiBusy = $state(false);
  aiError = $state<string | null>(null);
  aiLastRationale = $state<string | null>(null);
  /** Dernier screenshot de verification renvoye par l'agent Rust (data URL JPEG). */
  aiLastVerificationImage = $state<string | null>(null);
  /** Anti-spam cote client : timestamp ms du dernier sendAiCommand. */
  aiLastSentAtMs = $state(0);
  /** File des approbations shell en attente (FIFO). Le premier élément
   *  est affiché dans le modal — l'IA séquentielle ne produira jamais
   *  deux demandes simultanées mais on garde une liste par sécurité. */
  pendingShellApprovals = $state<PendingShellApproval[]>([]);

  // ── État boucle agentic ──────────────────────────────────────────────────
  /** Activable/désactivable par l'utilisateur (toggle UI futur). Quand
   *  désactivé, l'IA fonctionne en mono-shot comme avant : 1 commande
   *  utilisateur = 1 plan = aucun re-call automatique. */
  agenticEnabled = $state(true);
  /** Tour courant — 0 pour le premier appel, incrémenté à chaque relance
   *  automatique. Reset quand l'utilisateur saisit une nouvelle commande. */
  agenticIteration = $state(0);
  /** True pendant toute la durée d'une boucle multi-tour (de la commande
   *  utilisateur jusqu'au done=true / max / cancel). Permet à l'UI
   *  d'afficher "tour 2/5" et de distinguer "réflexion" vs "exécution". */
  agenticActive = $state(false);
  /** Compteur d'actions restantes pour le tour courant — décrémenté à chaque
   *  AI_ACTION_RESULT reçu. Quand il atteint 0, on déclenche la relance. */
  private agenticActionsRemaining = 0;
  /** Historique des tours précédents — envoyé à Gemini à chaque relance. */
  private agenticHistory: AiHistoryStep[] = [];
  /** Commande utilisateur d'origine — reutilisée à chaque tour (Gemini
   *  voit toujours le même objectif initial, et l'historique pour le contexte). */
  private agenticOriginalCommand: string | null = null;
  /** Buffer des résultats du tour courant — concat des stdout shell / status
   *  click pour donner du contexte à Gemini. Reset entre chaque tour. */
  private agenticCurrentTurnResults: string[] = [];
  /** Rationale + actions du tour courant, capturés au moment du envelope.
   *  Stockés pour les insérer dans l'historique au moment de la relance. */
  private agenticCurrentTurnRationale: string | null = null;
  private agenticCurrentTurnActionsJson: string = "[]";
  /** Dernière screenshot remontée par l'agent via AI_ACTION_RESULT — quand
   *  Gemini termine son plan par un screenshot, on le réutilise comme input
   *  du tour suivant (gain de temps + plus fiable que recapturer côté viewer). */
  private agenticLastAgentScreenshot: { b64: string; width: number; height: number } | null = null;

  private detachAiActionListener: (() => void) | null = null;
  private detachAiConnectionListener: (() => void) | null = null;
  private aiBusyWatchdogTimer: ReturnType<typeof setTimeout> | null = null;
  private pendingScreenshots = new Map<string, PendingScreenshot>();

  // ── Callbacks set by the orchestrator ────────────────────────────────────
  getSession: () => ControlSession | null = () => null;
  getSelectedFeature: () => "screen" | "chat" | "files" | null = () => null;
  getChatLocalRole: () => string = () => "viewer";
  getChatRemoteRole: () => string = () => "agent";
  getChatRoomId: () => string = () => "";
  resolveRoomId: () => string = () => "";
  pushChatMessage: (msg: ChatMessage) => void = () => {};
  getChatInput: () => string = () => "";
  setChatInput: (v: string) => void = () => {};
  /** Set rdScreenPlayRequested = true to auto-resume the video stream. */
  requestVideoResume: () => void = () => {};
  /** Read rdScreenPlayRequested. */
  isVideoPlayRequested: () => boolean = () => false;

  // ── Connection lifecycle ─────────────────────────────────────────────────
  connect = () => {
    if (this.detachAiActionListener) return; // already wired
    this.detachAiActionListener = this.client.onAction((env) => this.handleAiActionEnvelope(env));
    this.detachAiConnectionListener = this.client.onConnection((connected) => {
      this.aiConnected = connected;
      if (!connected) this.aiBusy = false;
    });
    void this.client.connect().catch((err) => {
      this.aiError = `Canal IA: ${String(err)}`;
    });
  };

  disconnect = () => {
    this.detachAiActionListener?.();
    this.detachAiActionListener = null;
    this.detachAiConnectionListener?.();
    this.detachAiConnectionListener = null;
    this.client.leaveSession();
    this.client.disconnect();
    this.aiConnected = false;
    this.aiBusy = false;
    this.stopAiBusyWatchdog();

    // Drain any pending screenshot requests so their timeouts don't fire
    // after the session ends (would resolve into a dead promise chain and
    // surface confusing "timeout" toasts mid-reconnect).
    for (const [, pending] of this.pendingScreenshots) {
      clearTimeout(pending.timeoutId);
      pending.reject(new Error("Session IA fermee avant reponse screenshot"));
    }
    this.pendingScreenshots.clear();

    // Vide la file d'approbations shell — l'agent Rust va timeout à 60s
    // et refuser par défaut, le viewer n'a plus à les afficher.
    this.pendingShellApprovals = [];

    // Coupe toute boucle agentic en cours — la session disparaît, plus de
    // raison de relancer l'IA.
    this.stopAgenticLoop("Session fermée");
  };

  // ── Kill switch + shell approval (P0 sécurité) ───────────────────────────
  /**
   * Envoie `AI_CANCEL` sur le DataChannel input. L'agent Rust marquera le
   * plan en cours comme annulé et la boucle dispatch() s'arrêtera entre
   * deux actions. `aiBusy` retombera quand l'agent renverra le résultat
   * "Plan annulé" ou que le watchdog 60s expirera.
   */
  sendAiCancel = (): boolean => {
    const channel = viewerPeer.viewerControlChannel;
    if (!channel || channel.readyState !== "open") {
      this.aiError = "Impossible d'annuler : DataChannel input fermé.";
      return false;
    }
    try {
      channel.send(JSON.stringify({ type: "AI_CANCEL" }));
      this.appendAiChatMessage(
        "🛑 Annulation demandée — l'IA s'arrêtera à la fin de l'action courante.",
        "ai-system"
      );
      // Optimistic UI : on libère immédiatement aiBusy pour permettre une
      // nouvelle commande dès que l'agent aura confirmé. Si l'agent ne
      // répond pas (channel cassé), le watchdog 60s catchera.
      this.aiBusy = false;
      this.stopAiBusyWatchdog();
      // Coupe aussi la boucle agentic — pas de relance après une annulation.
      this.stopAgenticLoop("Annulé par le technicien");
      return true;
    } catch (err) {
      this.aiError = `Envoi AI_CANCEL impossible : ${String(err)}`;
      return false;
    }
  };

  /**
   * Appelé par viewer-peer quand un `AI_SHELL_REQUEST` arrive sur le
   * DataChannel input. Push la demande dans la file — l'UI (modal)
   * réagit sur le state. L'agent attend 60s avant de timeout côté Rust.
   */
  handleShellRequest = (payload: Record<string, unknown>) => {
    const actionId = typeof payload.actionId === "string" ? payload.actionId : "";
    const cmd = typeof payload.cmd === "string" ? payload.cmd : "";
    const shell = typeof payload.shell === "string" ? payload.shell : "powershell";
    if (!actionId || !cmd) {
      console.warn("[ai] AI_SHELL_REQUEST invalide (actionId/cmd manquant)", payload);
      return;
    }
    // Anti-doublon : si l'actionId est déjà dans la file (re-livraison réseau),
    // ne pas push une seconde fois.
    if (this.pendingShellApprovals.some((p) => p.actionId === actionId)) {
      return;
    }
    this.pendingShellApprovals = [
      ...this.pendingShellApprovals,
      { actionId, cmd, shell, receivedAt: Date.now() }
    ];
    this.appendAiChatMessage(
      `🔐 L'IA demande l'autorisation d'exécuter une commande shell. Vérifie le modal.`,
      "ai-system"
    );
  };

  /**
   * Renvoie la décision (approuvé / refusé) à l'agent Rust et retire la
   * demande de la file. Si le DataChannel est fermé, on remonte une erreur
   * mais on retire quand même la demande (sinon le modal reste bloqué).
   */
  respondShellApproval = (actionId: string, approved: boolean): boolean => {
    this.pendingShellApprovals = this.pendingShellApprovals.filter(
      (p) => p.actionId !== actionId
    );
    const channel = viewerPeer.viewerControlChannel;
    if (!channel || channel.readyState !== "open") {
      this.aiError = "Impossible de renvoyer la décision : DataChannel fermé.";
      return false;
    }
    try {
      channel.send(JSON.stringify({
        type: "AI_SHELL_DECISION",
        actionId,
        approved
      }));
      return true;
    } catch (err) {
      this.aiError = `Envoi AI_SHELL_DECISION impossible : ${String(err)}`;
      return false;
    }
  };

  // ── Busy watchdog ────────────────────────────────────────────────────────
  startAiBusyWatchdog = () => {
    this.stopAiBusyWatchdog();
    this.aiBusyWatchdogTimer = setTimeout(() => {
      this.aiBusyWatchdogTimer = null;
      if (!this.aiBusy) return;
      this.aiBusy = false;
      this.aiError =
        "Aucune reponse du serveur apres 60s. " +
        "Verifie les logs Spring : la requete est probablement bloquee cote Gemini ou le retour STOMP est casse.";
      this.appendAiChatMessage(`❌ ${this.aiError}`, "ai-system");
    }, AI_BUSY_WATCHDOG_MS);
  };

  stopAiBusyWatchdog = () => {
    if (this.aiBusyWatchdogTimer) {
      clearTimeout(this.aiBusyWatchdogTimer);
      this.aiBusyWatchdogTimer = null;
    }
  };

  // ── Frame capture ────────────────────────────────────────────────────────
  captureFrame = (): { jpegBase64: string; width: number; height: number } | null => {
    const video = viewerPeer.viewerVideoEl;
    if (!video) return null;
    const w = video.videoWidth;
    const h = video.videoHeight;
    if (!w || !h) return null;

    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) return null;
    try {
      ctx.drawImage(video, 0, 0, w, h);
    } catch (err) {
      console.warn("[ai] captureFrame drawImage failed", err);
      return null;
    }
    const dataUrl = canvas.toDataURL("image/jpeg", 0.7);
    const comma = dataUrl.indexOf(",");
    if (comma < 0) return null;
    return { jpegBase64: dataUrl.slice(comma + 1), width: w, height: h };
  };

  waitForFreshFrame = async (timeoutMs = 3000): Promise<boolean> => {
    const video = viewerPeer.viewerVideoEl;
    if (!video) return false;

    const t0 = performance.now();
    const startTime = video.currentTime;

    while (performance.now() - t0 < timeoutMs) {
      const ok =
        video.videoWidth > 0 &&
        video.videoHeight > 0 &&
        video.readyState >= 2 &&
        video.currentTime > startTime;
      if (ok) return true;
      await new Promise((r) => setTimeout(r, 80));
    }
    return false;
  };

  captureFrameWithAutoResume = async (): Promise<
    { jpegBase64: string; width: number; height: number } | null
  > => {
    const wasPaused = !this.isVideoPlayRequested();
    if (wasPaused) {
      console.log("[ai] tier 2: auto-resume diffusion video…");
      this.requestVideoResume();
    }
    const freshFrame = await this.waitForFreshFrame(wasPaused ? 3500 : 1200);
    if (!freshFrame) {
      console.warn("[ai] tier 2: aucune frame fraiche disponible");
      return null;
    }
    return this.captureFrame();
  };

  // ── Remote screenshot (tier 1) ───────────────────────────────────────────
  requestScreenshotFromRemote = (
    timeoutMs = 5000
  ): Promise<{ jpegBase64: string; width: number; height: number }> => {
    const channel = viewerPeer.viewerControlChannel;
    if (!channel || channel.readyState !== "open") {
      return Promise.reject(new Error(
        `DataChannel "input" non disponible (state=${channel?.readyState ?? "null"})`
      ));
    }

    const commandId = (typeof crypto !== "undefined" && "randomUUID" in crypto)
      ? crypto.randomUUID()
      : `cmd-${Date.now()}-${Math.floor(Math.random() * 1e9).toString(36)}`;

    const payload = JSON.stringify({ type: "request_screenshot", commandId });
    console.log(`[ai] ▶ request_screenshot envoyé (commandId=${commandId}, len=${payload.length})`);

    return new Promise((resolve, reject) => {
      const timeoutId = setTimeout(() => {
        this.pendingScreenshots.delete(commandId);
        console.warn(
          `[ai] ✖ timeout apres ${timeoutMs}ms pour commandId=${commandId}.`
        );
        reject(new Error(
          `Le PC distant ne repond pas (timeout ${timeoutMs}ms). ` +
            "Cause la plus probable : l'agent Rust n'a pas le handler 'request_screenshot'."
        ));
      }, timeoutMs);

      this.pendingScreenshots.set(commandId, { resolve, reject, timeoutId });

      try {
        channel.send(payload);
      } catch (err) {
        clearTimeout(timeoutId);
        this.pendingScreenshots.delete(commandId);
        reject(new Error(`Envoi request_screenshot impossible : ${String(err)}`));
      }
    });
  };

  handleScreenshotResponse = (payload: Record<string, unknown>) => {
    const commandId = typeof payload.commandId === "string" ? payload.commandId : "";
    if (!commandId) return;
    const pending = this.pendingScreenshots.get(commandId);
    if (!pending) {
      console.warn(`[ai] screenshot msg avec commandId inconnu : ${commandId}`);
      return;
    }

    const type = typeof payload.type === "string" ? payload.type : "";

    switch (type) {
      case "screenshot_chunk_start": {
        const totalChunks = typeof payload.totalChunks === "number" ? payload.totalChunks : 0;
        pending.expectedChunks = totalChunks;
        pending.receivedChunks = new Array(totalChunks).fill("");
        pending.width = typeof payload.width === "number" ? payload.width : 0;
        pending.height = typeof payload.height === "number" ? payload.height : 0;
        const totalKb = typeof payload.totalBytes === "number"
          ? Math.round(payload.totalBytes / 1024)
          : 0;
        console.log(`[ai] chunk_start: ${totalChunks} chunks attendus (${totalKb} KB total, ${pending.width}x${pending.height})`);
        return;
      }

      case "screenshot_chunk": {
        if (!pending.receivedChunks) {
          console.warn(`[ai] chunk recu avant chunk_start pour ${commandId}`);
          return;
        }
        const index = typeof payload.index === "number" ? payload.index : -1;
        const data = typeof payload.data === "string" ? payload.data : "";
        if (index < 0 || index >= pending.receivedChunks.length) {
          console.warn(`[ai] chunk index ${index} hors limites (max ${pending.receivedChunks.length})`);
          return;
        }
        pending.receivedChunks[index] = data;
        return;
      }

      case "screenshot_chunk_end": {
        if (!pending.receivedChunks) {
          clearTimeout(pending.timeoutId);
          this.pendingScreenshots.delete(commandId);
          pending.reject(new Error("chunk_end recu sans chunk_start"));
          return;
        }
        const missing = pending.receivedChunks.findIndex((c) => c === "");
        if (missing !== -1 && pending.expectedChunks && missing < pending.expectedChunks) {
          clearTimeout(pending.timeoutId);
          this.pendingScreenshots.delete(commandId);
          pending.reject(new Error(`Chunk manquant a l'index ${missing}/${pending.expectedChunks}`));
          return;
        }
        const fullBase64 = pending.receivedChunks.join("");
        clearTimeout(pending.timeoutId);
        this.pendingScreenshots.delete(commandId);
        console.log(`[ai] chunk_end: reassemble ${fullBase64.length} chars (${Math.round(fullBase64.length / 1024)} KB)`);
        pending.resolve({
          jpegBase64: fullBase64,
          width: pending.width ?? 0,
          height: pending.height ?? 0,
        });
        return;
      }

      case "screenshot_response_error": {
        clearTimeout(pending.timeoutId);
        this.pendingScreenshots.delete(commandId);
        const err = typeof payload.error === "string" ? payload.error : "agent capture failed";
        pending.reject(new Error(err));
        return;
      }

      case "screenshot_response": {
        clearTimeout(pending.timeoutId);
        this.pendingScreenshots.delete(commandId);
        if (typeof payload.error === "string" && payload.error.length > 0) {
          pending.reject(new Error(payload.error));
          return;
        }
        const data = typeof payload.data === "string" ? payload.data : "";
        if (!data) {
          pending.reject(new Error("Reponse screenshot vide"));
          return;
        }
        const width = typeof payload.width === "number" ? payload.width : 0;
        const height = typeof payload.height === "number" ? payload.height : 0;
        pending.resolve({ jpegBase64: data, width, height });
        return;
      }

      default:
        console.warn(`[ai] screenshot msg avec type inconnu : "${type}"`);
        return;
    }
  };

  // ── Chat-bound AI message ────────────────────────────────────────────────
  appendAiChatMessage = (content: string, role: "ai-system" | "ai-user" | "ai-action") => {
    const localRole = this.getChatLocalRole();
    const remoteRole = this.getChatRemoteRole();
    const senderRole = role === "ai-user" ? localRole : remoteRole;
    const senderName = role === "ai-user" ? "Technicien (IA)" : "Agent IA";
    const msg: ChatMessage = {
      roomId: this.getChatRoomId() || this.resolveRoomId() || "ai",
      senderRole,
      senderName,
      receiverRole: role === "ai-user" ? remoteRole : localRole,
      receiverName: role === "ai-user" ? remoteRole : localRole,
      content,
      timestamp: new Date(Date.now()).toISOString()
    };
    this.pushChatMessage(msg);
  };

  formatAiActionForChat = (a: AiAction): string => {
    switch (a.type) {
      case "click":         return `🖱️ click(${a.x.toFixed(2)}, ${a.y.toFixed(2)}${a.button ? `, ${a.button}` : ""})`;
      case "double_click":  return `🖱️🖱️ double_click(${a.x.toFixed(2)}, ${a.y.toFixed(2)})`;
      case "move":          return `➡️ move(${a.x.toFixed(2)}, ${a.y.toFixed(2)})`;
      case "type_text":     return `⌨️ type "${a.text}"`;
      case "key":           return `🔑 key ${[...(a.modifiers ?? []), a.key].join("+")}`;
      case "shell":         return `💻 shell(${a.shell ?? "default"}): ${a.cmd}`;
      case "screenshot":    return `📸 screenshot`;
      case "wait":          return `⏳ wait ${a.ms}ms`;
      case "scroll": {
        const dir = a.dy > 0 ? "↓" : a.dy < 0 ? "↑" : (a.dx && a.dx > 0 ? "→" : "←");
        const loc = a.x !== undefined && a.y !== undefined ? ` @ (${a.x.toFixed(2)},${a.y.toFixed(2)})` : "";
        return `🖱️ scroll ${dir} ${Math.abs(a.dy || a.dx || 0)}${loc}`;
      }
      case "drag":          return `🖐️ drag (${a.x.toFixed(2)},${a.y.toFixed(2)}) → (${a.destX.toFixed(2)},${a.destY.toFixed(2)})`;
      default:              return `❓ ${JSON.stringify(a)}`;
    }
  };

  forwardAiActionToAgent = (action: AiAction): boolean => {
    const channel = viewerPeer.viewerControlChannel;
    if (!channel || channel.readyState !== "open") return false;
    try {
      channel.send(JSON.stringify({ type: "AI_ACTION", action }));
      return true;
    } catch (err) {
      console.warn("[ai] forward to agent failed", err);
      return false;
    }
  };

  handleAiActionResult = (payload: Record<string, unknown>) => {
    const result = payload as unknown as AiActionResult;
    const action = result.action ?? "?";
    const ok = !!result.ok;
    const message = (result.message ?? "").trim();

    // Screenshot : on garde l'image pour l'affichage ET pour la relance agentic
    // (réutiliser le screenshot post-action au lieu d'en capturer un nouveau
    // côté viewer = plus fiable, déjà décodé par l'agent au bon timing).
    if (action === "screenshot" && result.screenshot) {
      this.aiLastVerificationImage = `data:image/jpeg;base64,${result.screenshot}`;
      if (this.agenticActive) {
        // Dimensions ne sont pas dans le payload pour le screenshot de vérif,
        // on met 0x0 — le backend connaît déjà la résolution via le 1er tour.
        this.agenticLastAgentScreenshot = { b64: result.screenshot, width: 0, height: 0 };
        this.agenticCurrentTurnResults.push("screenshot OK");
      }
      this.tickAgenticActionCompleted();
      return;
    }

    // Pour les actions VISUELLES (click, type_text, key, scroll, drag, move),
    // les succès ne sont pas affichés — le technicien voit le résultat sur le
    // flux vidéo et l'historique d'actions verbeux polluait l'UI.
    //
    // Pour les actions SANS sortie visuelle (shell), le stdout EST le résultat
    // utile (sortie ipconfig, version Windows, etc.). On l'affiche toujours,
    // succès ou échec — sinon la commande IA "donne mon IP" exécute ipconfig
    // mais le technicien ne voit rien (le shell est invisible avec
    // CREATE_NO_WINDOW côté agent).
    const isShellAction = action === "shell";

    if (!ok) {
      this.appendAiChatMessage(
        `❌ ${action} a echoue : ${message || "erreur inconnue"}`,
        "ai-system"
      );
      // L'action "cancel" est émise par l'agent quand le plan est interrompu
      // par AI_CANCEL — on arrête toute la boucle agentic ici.
      if (action === "cancel" && this.agenticActive) {
        this.stopAgenticLoop("Plan annulé par le technicien");
        return;
      }
      if (this.agenticActive) {
        this.agenticCurrentTurnResults.push(`${action} FAILED: ${message}`);
      }
    } else if (isShellAction && message) {
      // Format pretty-print du résultat shell : on retire le "exit=0\n" qui
      // n'apporte rien quand la commande a réussi, et on garde stdout/stderr.
      const cleaned = message
        .replace(/^exit=0\n?/, "")
        .replace(/^stdout:\s*/, "")
        .trim();
      this.appendAiChatMessage(
        `💻 Résultat shell :\n${cleaned}`,
        "ai-system"
      );
      if (this.agenticActive) {
        // Le stdout est crucial pour le tour suivant — Gemini doit lire le
        // résultat d'ipconfig pour décider s'il a fini.
        this.agenticCurrentTurnResults.push(`${action}: ${cleaned}`);
      }
    } else if (this.agenticActive) {
      // Click/type/key réussi : note minimal pour l'historique.
      this.agenticCurrentTurnResults.push(`${action} OK`);
    }

    this.tickAgenticActionCompleted();
  };

  handleAiActionEnvelope = (env: AiActionEnvelope) => {
    this.stopAiBusyWatchdog();
    this.aiLastRationale = env.rationale ?? null;

    const current = this.getSession();
    if (current && String(current.id) !== env.sessionId) {
      // L'envelope concerne une autre session — on ignore mais on ne touche
      // pas aiBusy : si on a une autre boucle en cours, elle reste active.
      return;
    }

    if (env.status !== "ok") {
      this.aiError = env.error ?? "Erreur IA inconnue.";
      this.appendAiChatMessage(`❌ ${this.aiError}`, "ai-system");
      this.stopAgenticLoop(this.aiError);
      return;
    }
    this.aiError = null;

    if (env.rationale) {
      this.appendAiChatMessage(`🧠 ${env.rationale}`, "ai-system");
    }

    const actions = env.actions ?? [];
    const isDone = !!env.done;

    // Cas 1 — boucle agentic active : on capture le contexte pour l'historique
    // et on décide si on relance après exécution.
    if (this.agenticActive) {
      this.agenticCurrentTurnRationale = env.rationale ?? null;
      try {
        this.agenticCurrentTurnActionsJson = JSON.stringify(actions);
      } catch {
        this.agenticCurrentTurnActionsJson = "[]";
      }
      this.agenticCurrentTurnResults = [];
      this.agenticLastAgentScreenshot = null;

      if (isDone && actions.length === 0) {
        // Gemini a fini sans actions supplémentaires — la rationale est la
        // réponse finale. Boucle terminée proprement.
        this.stopAgenticLoop(null);
        return;
      }

      if (this.agenticIteration + 1 >= AGENTIC_MAX_ITERATIONS && !isDone) {
        // On va faire ce tour mais ce sera le dernier — on prévient Gemini
        // dans le chat technicien et on coupera la relance après les résultats.
        this.appendAiChatMessage(
          `⚠️ Tour ${this.agenticIteration + 1}/${AGENTIC_MAX_ITERATIONS} — dernier tour avant arrêt automatique.`,
          "ai-system"
        );
      }

      // En agentic, on attend que TOUTES les actions du tour aient remonté
      // leur AI_ACTION_RESULT avant de relancer. Si actions vide + done=false,
      // on relance quand même (Gemini réfléchit sans agir — autorisé 1 tour).
      this.agenticActionsRemaining = actions.length;
      if (isDone) {
        // done=true + actions non vides = dernière exécution avec réponse
        // finale, on arrêtera la boucle après l'exécution.
        this.agenticDoneAfterTurn = true;
      }
    } else {
      // Cas 2 — mono-shot classique : libère aiBusy tout de suite.
      this.aiBusy = false;
    }

    // Envoi des actions à l'agent Rust.
    for (const action of actions) {
      const ok = this.forwardAiActionToAgent(action);
      if (!ok) {
        this.appendAiChatMessage(
          `⚠️ Impossible d'envoyer cette action a l'agent (DataChannel indisponible).`,
          "ai-system"
        );
        if (this.agenticActive) {
          this.stopAgenticLoop("DataChannel agent fermé");
        }
        break;
      }
    }

    // Cas "actions vide" en mode agentic : Gemini "réfléchit" sans action.
    // On déclenche immédiatement la relance (rien à attendre).
    if (this.agenticActive && actions.length === 0 && !isDone) {
      this.tickAgenticActionCompleted();
    }
  };

  /** True quand `done=true` est arrivé MAIS qu'il reste des actions à
   *  exécuter — on arrête la boucle après leur complétion. */
  private agenticDoneAfterTurn = false;

  /**
   * Décrémente le compteur d'actions restantes du tour courant. Quand il
   * atteint 0, on décide : relance auto, arrêt par done, ou arrêt par max.
   */
  private tickAgenticActionCompleted = () => {
    if (!this.agenticActive) return;
    if (this.agenticActionsRemaining > 0) {
      this.agenticActionsRemaining--;
    }
    if (this.agenticActionsRemaining > 0) return;

    // Tour complet. Décision :
    if (this.agenticDoneAfterTurn) {
      this.stopAgenticLoop(null);
      return;
    }
    if (this.agenticIteration + 1 >= AGENTIC_MAX_ITERATIONS) {
      this.appendAiChatMessage(
        `🛑 Limite de ${AGENTIC_MAX_ITERATIONS} tours atteinte — arrêt automatique.`,
        "ai-system"
      );
      this.stopAgenticLoop(null);
      return;
    }
    // Sinon : on push le tour dans l'historique et on relance.
    this.pushCurrentTurnToHistory();
    this.agenticIteration++;
    setTimeout(() => {
      // Re-check : l'utilisateur a peut-être cliqué Stop pendant le délai.
      if (!this.agenticActive) return;
      void this.dispatchNextAgenticTurn();
    }, AGENTIC_INTER_TURN_DELAY_MS);
  };

  private pushCurrentTurnToHistory = () => {
    const resultText = this.agenticCurrentTurnResults
      .join(" | ")
      .slice(0, AGENTIC_RESULT_TEXT_MAX);
    this.agenticHistory.push({
      iteration: this.agenticIteration,
      rationale: this.agenticCurrentTurnRationale,
      actionsJson: this.agenticCurrentTurnActionsJson,
      resultText
    });
  };

  /**
   * Lance le tour suivant : capture screenshot (de préférence celui remonté
   * par l'agent au tour précédent, sinon canvas local), puis publishFrame
   * avec l'historique. Bypass le cooldown 6s (c'est une relance auto, pas
   * une nouvelle commande utilisateur).
   */
  private dispatchNextAgenticTurn = async (): Promise<void> => {
    if (!this.agenticActive || !this.agenticOriginalCommand) return;
    const session = this.getSession();
    if (!session || session.status !== "ACTIVE") {
      this.stopAgenticLoop("Session inactive");
      return;
    }

    // Préfère la dernière screenshot remontée par l'agent (post-actions),
    // sinon retombe sur la capture viewer canvas / agent direct.
    let frame: { jpegBase64: string; width: number; height: number };
    if (this.agenticLastAgentScreenshot?.b64) {
      frame = {
        jpegBase64: this.agenticLastAgentScreenshot.b64,
        width: this.agenticLastAgentScreenshot.width,
        height: this.agenticLastAgentScreenshot.height
      };
    } else {
      // Pas de screenshot fraîche dans le tour précédent — on en redemande
      // une à l'agent. Si l'agent ne répond pas (tier 1 KO), on prend le
      // canvas viewer (tier 2).
      try {
        const remote = await this.requestScreenshotFromRemote(3000);
        frame = remote;
      } catch {
        const local = await this.captureFrameWithAutoResume();
        if (!local) {
          this.stopAgenticLoop("Capture impossible pour la relance agentic");
          return;
        }
        frame = local;
      }
    }

    this.aiBusy = true;
    this.startAiBusyWatchdog();
    console.log(
      `[ai] 🔁 tour agentic ${this.agenticIteration}/${AGENTIC_MAX_ITERATIONS}`
    );

    const ok = await this.client.publishFrame({
      sessionId: String(session.id),
      command: this.agenticOriginalCommand,
      screenshot: frame.jpegBase64,
      frameWidth: frame.width,
      frameHeight: frame.height,
      technicianUsername: session.technicianUsername ?? undefined,
      iteration: this.agenticIteration,
      history: this.agenticHistory
    });
    if (!ok) {
      this.stopAgenticLoop("Échec d'envoi du frame agentic");
    }
  };

  /**
   * Arrête proprement la boucle agentic. Reset tous les compteurs et libère
   * aiBusy pour permettre une nouvelle commande utilisateur.
   */
  private stopAgenticLoop = (reason: string | null) => {
    if (!this.agenticActive) return;
    this.agenticActive = false;
    this.agenticActionsRemaining = 0;
    this.agenticHistory = [];
    this.agenticOriginalCommand = null;
    this.agenticCurrentTurnResults = [];
    this.agenticDoneAfterTurn = false;
    this.agenticLastAgentScreenshot = null;
    this.aiBusy = false;
    this.stopAiBusyWatchdog();
    if (reason) {
      console.log(`[ai] 🔁 boucle agentic arrêtée : ${reason}`);
    }
  };

  // ── Commands ─────────────────────────────────────────────────────────────
  sendChatAsAi = async (): Promise<void> => {
    const content = this.getChatInput().trim();
    if (!content) return;
    if (this.getSelectedFeature() !== "screen") {
      this.aiError = "L'IA n'est disponible que depuis le panneau Ecran.";
      return;
    }
    this.setChatInput("");
    await this.sendAiCommand(content);
  };

  sendAiCommand = async (command: string): Promise<void> => {
    const trimmed = command.trim();
    if (!trimmed) return;

    const session = this.getSession();
    if (!session || session.status !== "ACTIVE") {
      this.aiError = "Aucune session active.";
      return;
    }
    if (!viewerPeer.viewerDataChannelOpen) {
      this.aiError = "DataChannel WebRTC pas encore ouvert.";
      return;
    }
    if (!this.client.isConnected()) {
      this.aiError = "Canal IA STOMP hors ligne — tentative de reconnexion…";
      void this.client.connect().catch(() => {});
      return;
    }

    const sinceLastMs = Date.now() - this.aiLastSentAtMs;
    if (sinceLastMs < AI_MIN_INTERVAL_MS) {
      const remainingS = Math.ceil((AI_MIN_INTERVAL_MS - sinceLastMs) / 1000);
      this.aiError = `Attends ${remainingS}s avant la prochaine commande IA (quota Gemini free-tier).`;
      return;
    }

    this.aiBusy = true;
    this.aiError = null;

    let frame: { jpegBase64: string; width: number; height: number };
    let captureSource: "remote-agent" | "local-canvas";

    try {
      console.log("[ai] tier 1: requestScreenshotFromRemote (3s timeout)…");
      frame = await this.requestScreenshotFromRemote(3000);
      captureSource = "remote-agent";
      console.log(`[ai] ✓ tier 1 OK (remote screenshot, ${frame.width}x${frame.height})`);
    } catch (remoteErr) {
      console.warn(`[ai] ✖ tier 1 KO (${(remoteErr as Error).message}). Fallback canvas…`);
      const fallback = await this.captureFrameWithAutoResume();
      if (!fallback) {
        this.aiBusy = false;
        this.aiError =
          "Capture impossible : ni l'agent distant ni le flux video local ne fournissent d'image. " +
          "Verifie que la diffusion video est active (clique Play sur l'ecran distant).";
        return;
      }
      frame = fallback;
      captureSource = "local-canvas";
      console.log(`[ai] ✓ tier 2 OK (canvas fallback, ${frame.width}x${frame.height})`);
      this.appendAiChatMessage(
        "⚠️ Capture via flux video local (l'agent distant n'a pas repondu). Qualite reduite.",
        "ai-system"
      );
    }
    this.aiLastSentAtMs = Date.now();
    console.log(`[ai] capture source: ${captureSource}`);
    this.startAiBusyWatchdog();

    this.appendAiChatMessage(`/ai ${trimmed}`, "ai-user");

    // Initialise l'état boucle agentic — sera désactivé par sendAiCancel,
    // par done=true côté Gemini, ou par AGENTIC_MAX_ITERATIONS atteint.
    if (this.agenticEnabled) {
      this.agenticActive = true;
      this.agenticIteration = 0;
      this.agenticHistory = [];
      this.agenticOriginalCommand = trimmed;
      this.agenticCurrentTurnResults = [];
      this.agenticCurrentTurnRationale = null;
      this.agenticCurrentTurnActionsJson = "[]";
      this.agenticLastAgentScreenshot = null;
      this.agenticDoneAfterTurn = false;
    }

    const ok = await this.client.publishFrame({
      sessionId: String(session.id),
      command: trimmed,
      screenshot: frame.jpegBase64,
      frameWidth: frame.width,
      frameHeight: frame.height,
      technicianUsername: session.technicianUsername ?? undefined,
      iteration: 0,
      history: []
    });
    if (!ok) {
      this.stopAgenticLoop("Echec d'envoi du frame IA");
      this.aiError = "Echec d'envoi du frame IA (REST + STOMP tous les deux KO). Verifie la connexion reseau.";
    }
  };
}

export const aiPipeline = new AiPipeline();
