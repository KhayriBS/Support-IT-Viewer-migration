import { Client, type IMessage } from "@stomp/stompjs";
import { technicianApi } from "$lib/api/technician-api";

/**
 * AI Agent action emitted by the backend after Gemini analyses the screenshot.
 * Coordinates are normalised in [0, 1] relative to the captured frame so the
 * remote agent can scale them to its real screen resolution.
 */
export type AiAction =
  | { type: "click"; x: number; y: number; button?: "left" | "right" | "middle" }
  | { type: "double_click"; x: number; y: number }
  | { type: "move"; x: number; y: number }
  | { type: "type_text"; text: string }
  | { type: "key"; key: string; modifiers?: string[] }
  | { type: "shell"; cmd: string; shell?: "cmd" | "powershell" | "bash" }
  | { type: "screenshot" }
  | { type: "wait"; ms: number }
  | { type: "scroll"; x?: number; y?: number; dy: number; dx?: number }
  | { type: "drag"; x: number; y: number; destX: number; destY: number; button?: "left" | "right" | "middle" };

export interface AiActionEnvelope {
  sessionId: string;
  command: string;
  status: "ok" | "error";
  error?: string | null;
  actions: AiAction[];
  rationale?: string | null;
}

export interface AiFrameRequest {
  sessionId: string;
  command: string;
  /** JPEG payload, base64-encoded, NO data: prefix. */
  screenshot: string;
  /** Native resolution of the source frame — used by the agent to denormalise clicks. */
  frameWidth: number;
  frameHeight: number;
  technicianUsername?: string;
}

export type AiConnectionState = "idle" | "connecting" | "connected" | "error";

function toWsBaseUrl(httpBase: string) {
  return httpBase
    .replace(/^http:/i, "ws:")
    .replace(/^https:/i, "wss:")
    .replace(/\/$/, "");
}

/**
 * Dedicated STOMP client for the AI agent channel.
 *
 * Reuses the existing /ws/chat STOMP endpoint exposed by Spring Boot — STOMP
 * destinations are independent of the chat ones, so no backend reconfig is
 * required beyond adding @MessageMapping("/ai/frame").
 *
 * Inbound: /user/queue/ai/actions (per-STOMP-session via SimpMessagingTemplate
 * .convertAndSendToUser).
 * Outbound: /app/ai/frame (AiFrameRequest payload).
 */
/** STOMP subscription handle (opaque ; on appelle juste unsubscribe()). */
interface StompSub {
  unsubscribe(): void;
}

export class AiRealtimeClient {
  private client: Client | null = null;
  private state: AiConnectionState = "idle";

  private actionHandlers = new Set<(env: AiActionEnvelope) => void>();
  private connectionHandlers = new Set<(connected: boolean) => void>();

  /** Topic subscription pour la session courante. Reabonne sur reconnect. */
  private topicSub: { sessionId: string; sub: StompSub } | null = null;
  /** Session id memorise pour reabonnement automatique apres reconnect. */
  private wantedSessionId: string | null = null;

  /**
   * Dedup window — le serveur publie le meme envelope sur DEUX canaux
   * (/user/queue/ai/actions + /topic/ai/<sessionId>) en garde-fou. On
   * deduplique cote front sur une fenetre glissante de 2s.
   */
  private recentEnvelopes = new Map<string, number>();

  getConnectionState(): AiConnectionState {
    return this.state;
  }

  isConnected(): boolean {
    return this.state === "connected" && !!this.client?.connected;
  }

  async connect(): Promise<void> {
    if (this.state === "connected") return;

    this._internalDisconnect(true);
    this._setState("connecting");

    const baseUrl = technicianApi.baseUrl.replace(/\/$/, "");
    const wsUrl = `${toWsBaseUrl(baseUrl)}/ws/chat`;
    const httpUrl = `${baseUrl}/ws/chat`;

    let sockjsCtor: (new (url: string) => WebSocket) | null = null;
    try {
      const mod = await import("sockjs-client");
      sockjsCtor = (mod as { default?: new (url: string) => WebSocket }).default
        ?? (mod as unknown as new (url: string) => WebSocket);
    } catch (err) {
      console.warn("[ai-stomp] sockjs-client unavailable, falling back to native WS", err);
    }

    this.client = new Client({
      reconnectDelay: 5000,
      heartbeatIncoming: 4000,
      heartbeatOutgoing: 4000,
      debug: (msg) => {
        if (typeof console !== "undefined") console.debug("[ai-stomp]", msg);
      },

      webSocketFactory: () => {
        if (sockjsCtor) {
          try {
            return new sockjsCtor(httpUrl);
          } catch (err) {
            console.warn("[ai-stomp] SockJS construct failed, fallback native WS", err);
          }
        }
        return new WebSocket(wsUrl);
      },

      onConnect: () => {
        // Voie 1 : user-destination (sujet aux problemes de Principal/sessionId)
        this.client?.subscribe("/user/queue/ai/actions", (frame: IMessage) => {
          this._dispatchEnvelope(frame.body, "/user/queue/ai/actions");
        });
        this.client?.subscribe("/user/queue/ai/error", (frame: IMessage) => {
          this._dispatchEnvelope(frame.body, "/user/queue/ai/error");
        });

        // Voie 2 : topic dedie a la session (resouscrit auto apres reconnect).
        if (this.wantedSessionId) {
          this._subscribeTopic(this.wantedSessionId);
        }

        this._setState("connected");
      },

      onDisconnect: () => {
        if (this.state !== "idle") this._setState("idle");
      },

      onStompError: () => this._setState("error"),
      onWebSocketError: () => this._setState("error")
    });

    this.client.activate();
  }

  disconnect(): void {
    this._internalDisconnect(false);
  }

  private _internalDisconnect(silent: boolean): void {
    if (this.client?.active) {
      try { this.topicSub?.sub.unsubscribe(); } catch { /* idem */ }
      void this.client.deactivate();
    }
    this.client = null;
    this.topicSub = null;
    if (!silent && this.state !== "idle") this._setState("idle");
    else this.state = "idle";
  }

  /**
   * S'abonne au topic dedie d'une session — appele par le viewer quand la
   * session devient ACTIVE. Si la session change, on se desabonne de
   * l'ancien topic avant.
   *
   * Voie de secours principale au cas ou /user/queue/ai/actions ne route
   * pas (probleme de Principal absent, simpSessionId mismatch, etc.).
   */
  joinSession(sessionId: string): void {
    if (!sessionId) return;
    this.wantedSessionId = sessionId;
    if (this.client?.connected) {
      this._subscribeTopic(sessionId);
    }
    // Si pas encore connecte, le subscribe sera fait dans onConnect.
  }

  leaveSession(): void {
    this.wantedSessionId = null;
    try { this.topicSub?.sub.unsubscribe(); } catch { /* ignore */ }
    this.topicSub = null;
  }

  private _subscribeTopic(sessionId: string): void {
    if (!this.client?.connected) return;
    if (this.topicSub?.sessionId === sessionId) return; // deja abonne

    // Desabonnement de l'ancien topic si on change de session
    if (this.topicSub) {
      try { this.topicSub.sub.unsubscribe(); } catch { /* ignore */ }
      this.topicSub = null;
    }

    const dest = `/topic/ai/${sessionId}`;
    const sub = this.client.subscribe(dest, (frame: IMessage) => {
      this._dispatchEnvelope(frame.body, dest);
    });
    this.topicSub = { sessionId, sub };
    console.log(`[ai-stomp] subscribed to ${dest}`);
  }

  /**
   * Decode + deduplique un frame entrant. Le serveur publie SCIEMMENT le
   * meme envelope sur 2 destinations (defensive design) : on receptionne
   * potentiellement le meme paquet deux fois, separes de quelques ms. On
   * trash le doublon sur une fenetre de 2 secondes.
   */
  private _dispatchEnvelope(body: string, source: string): void {
    let env: AiActionEnvelope;
    try {
      env = JSON.parse(body) as AiActionEnvelope;
    } catch (err) {
      console.warn(`[ai-stomp] malformed frame on ${source}`, err);
      return;
    }

    // Cle de dedup : sessionId|command|rationale|status. Si plusieurs actions
    // identiques arrivent dans la meme fenetre, c'est le meme envelope.
    const dedupKey =
      `${env.sessionId ?? ""}|${env.command ?? ""}|${env.rationale ?? ""}|${env.status}`;
    const now = Date.now();

    // Purge des entrees expirees (> 2s) — eviter une croissance illimitee.
    for (const [k, t] of this.recentEnvelopes) {
      if (now - t > 2000) this.recentEnvelopes.delete(k);
    }

    const prev = this.recentEnvelopes.get(dedupKey);
    if (prev !== undefined && now - prev < 2000) {
      console.log(`[ai-stomp] dedup: skipped duplicate envelope from ${source}`);
      return;
    }
    this.recentEnvelopes.set(dedupKey, now);

    console.log(`[ai-stomp] ◀ envelope from ${source} (status=${env.status}, actions=${env.actions?.length ?? 0})`);
    this.actionHandlers.forEach((h) => h(env));
  }

  private _setState(next: AiConnectionState): void {
    this.state = next;
    if (next !== "connecting") {
      const connected = next === "connected";
      this.connectionHandlers.forEach((h) => h(connected));
    }
  }

  /**
   * Publishes a frame + command to the backend.
   *
   * Strategie en deux temps :
   *   1. HTTP POST /ai/frame  ← voie principale, aucune limite de taille
   *      (le screenshot fait ~95 KB ce qui est jete par certains proxys
   *      WebSocket — Render free-tier notamment)
   *   2. STOMP /app/ai/frame  ← fallback si REST echoue (rare)
   *
   * Dans les deux cas, la reponse Gemini arrive via STOMP sur
   * /topic/ai/<sessionId> + /user/queue/ai/actions. Le client doit etre
   * abonne AVANT d'appeler publishFrame (cf joinSession).
   */
  async publishFrame(req: AiFrameRequest): Promise<boolean> {
    const baseUrl = technicianApi.baseUrl.replace(/\/$/, "");
    const bodyStr = JSON.stringify(req);
    const sizeKb = Math.round(bodyStr.length / 1024);

    // ─── Tentative 1 : REST POST ─────────────────────────────────────
    try {
      console.log(`[ai-stomp] ▶ POST ${baseUrl}/ai/frame (${sizeKb} KB)`);
      const response = await fetch(`${baseUrl}/ai/frame`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: bodyStr,
        // 15s pour absorber les cold-starts Render free-tier.
        signal: AbortSignal.timeout(15_000)
      });

      if (response.ok) {
        const result = (await response.json().catch(() => ({}))) as Record<string, unknown>;
        console.log(`[ai-stomp] ✓ POST accepted, requestId=${result.requestId}, responseChannel=${result.responseChannel}`);
        return true;
      }
      console.warn(`[ai-stomp] REST POST returned ${response.status} ${response.statusText}, falling back to STOMP`);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.warn(`[ai-stomp] REST POST failed (${msg}), falling back to STOMP`);
    }

    // ─── Fallback : STOMP /app/ai/frame ──────────────────────────────
    if (!this.isConnected()) {
      console.error("[ai-stomp] STOMP fallback impossible — pas connecte");
      return false;
    }
    try {
      this.client!.publish({
        destination: "/app/ai/frame",
        body: bodyStr
      });
      console.log("[ai-stomp] ▶ STOMP fallback frame envoyé");
      return true;
    } catch (err) {
      console.error("[ai-stomp] STOMP publish failed:", err);
      return false;
    }
  }

  onAction(handler: (env: AiActionEnvelope) => void): () => void {
    this.actionHandlers.add(handler);
    return () => this.actionHandlers.delete(handler);
  }

  onConnection(handler: (connected: boolean) => void): () => void {
    this.connectionHandlers.add(handler);
    return () => this.connectionHandlers.delete(handler);
  }
}
