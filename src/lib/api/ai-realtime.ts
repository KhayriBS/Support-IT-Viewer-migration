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
  | { type: "wait"; ms: number };

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
export class AiRealtimeClient {
  private client: Client | null = null;
  private state: AiConnectionState = "idle";

  private actionHandlers = new Set<(env: AiActionEnvelope) => void>();
  private connectionHandlers = new Set<(connected: boolean) => void>();

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
        this.client?.subscribe("/user/queue/ai/actions", (frame: IMessage) => {
          try {
            const payload = JSON.parse(frame.body) as AiActionEnvelope;
            this.actionHandlers.forEach((h) => h(payload));
          } catch (err) {
            console.warn("[ai-stomp] malformed action frame", err);
          }
        });

        // Canal dedie pour les erreurs serveur (timeout 30s Gemini, exceptions
        // inattendues). Spring pousse aussi l'erreur sur /actions en duplicate
        // pour compat, mais ce canal permet de declencher une UI specifique
        // (ex: banner rouge persistante au lieu d'une bulle dans le chat).
        this.client?.subscribe("/user/queue/ai/error", (frame: IMessage) => {
          try {
            const payload = JSON.parse(frame.body) as AiActionEnvelope;
            this.actionHandlers.forEach((h) => h(payload));
          } catch (err) {
            console.warn("[ai-stomp] malformed error frame", err);
          }
        });

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
      void this.client.deactivate();
    }
    this.client = null;
    if (!silent && this.state !== "idle") this._setState("idle");
    else this.state = "idle";
  }

  private _setState(next: AiConnectionState): void {
    this.state = next;
    if (next !== "connecting") {
      const connected = next === "connected";
      this.connectionHandlers.forEach((h) => h(connected));
    }
  }

  /**
   * Publishes a frame + command to the backend. Returns false if STOMP is not
   * connected — caller should surface a UI error instead of silently dropping.
   */
  publishFrame(req: AiFrameRequest): boolean {
    if (!this.isConnected()) return false;
    this.client!.publish({
      destination: "/app/ai/frame",
      body: JSON.stringify(req)
    });
    return true;
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
