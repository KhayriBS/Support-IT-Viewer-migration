import { Client, type IMessage } from "@stomp/stompjs";
import { technicianApi } from "$lib/api/technician-api";
import type { ChatMessage, TypingNotification } from "$lib/api/types";

export type ChatConnectionState = "idle" | "connecting" | "connected" | "error";

function toWsBaseUrl(httpBase: string) {
  return httpBase
    .replace(/^http:/i, "ws:")
    .replace(/^https:/i, "wss:")
    .replace(/\/$/, "");
}

export class ChatRealtimeClient {
  private client: Client | null = null;
  private currentRoomId: string | null = null;
  private state: ChatConnectionState = "idle";

  private messageHandlers = new Set<(msg: ChatMessage) => void>();
  private typingHandlers = new Set<(msg: TypingNotification) => void>();
  private connectionHandlers = new Set<(connected: boolean) => void>();

  // ── Public read-only accessors ───────────────────────────────────────────

  getCurrentRoomId(): string | null {
    return this.currentRoomId;
  }

  getConnectionState(): ChatConnectionState {
    return this.state;
  }

  isConnected(): boolean {
    return this.state === "connected" && !!this.client?.connected;
  }

  // ── Connect ──────────────────────────────────────────────────────────────

  async connect(roomId: string): Promise<void> {
    // Idempotent: same room already connected
    if (this.state === "connected" && this.currentRoomId === roomId) {
      return;
    }

    // Room changed or not connected: clean slate
    this._internalDisconnect(/* silent */ true);
    this.currentRoomId = roomId;
    this._setState("connecting");

    const baseUrl = technicianApi.baseUrl.replace(/\/$/, "");
    const wsUrl = `${toWsBaseUrl(baseUrl)}/ws/chat`;
    const httpUrl = `${baseUrl}/ws/chat`;

    // Charge SockJS en avance — le backend Spring expose `/ws/chat` avec
    // `.withSockJS()` ; un upgrade WebSocket "nu" sur la même URL est routé
    // vers le handler SockJS qui attend le protocole SockJS, ce qui donne
    // « HORS LIGNE ». Avec SockJS-client on parle le bon protocole.
    let sockjsCtor: (new (url: string) => WebSocket) | null = null;
    try {
      const mod = await import("sockjs-client");
      sockjsCtor = (mod as { default?: new (url: string) => WebSocket }).default
        ?? (mod as unknown as new (url: string) => WebSocket);
    } catch (err) {
      console.warn("[stomp] sockjs-client unavailable, falling back to native WS", err);
    }

    this.client = new Client({
      reconnectDelay: 5000,
      heartbeatIncoming: 4000,
      heartbeatOutgoing: 4000,
      // Verbose STOMP logs en dev pour diagnostiquer "HORS LIGNE"
      debug: (msg) => {
        if (typeof console !== "undefined") console.debug("[stomp]", msg);
      },

      webSocketFactory: () => {
        if (sockjsCtor) {
          try {
            return new sockjsCtor(httpUrl);
          } catch (err) {
            console.warn("[stomp] SockJS construct failed, fallback native WS", err);
          }
        }
        return new WebSocket(wsUrl);
      },

      onConnect: () => {
        // Called on first connect AND after each successful reconnect.
        // Re-subscribing here is correct — STOMP.js clears subscriptions on disconnect.
        this.client?.subscribe(`/topic/chat/${roomId}`, (frame: IMessage) => {
          try {
            const payload = JSON.parse(frame.body) as ChatMessage;
            this.messageHandlers.forEach((h) => h(payload));
          } catch {
            // ignore malformed frames
          }
        });

        this.client?.subscribe(`/topic/chat/${roomId}/typing`, (frame: IMessage) => {
          try {
            const payload = JSON.parse(frame.body) as TypingNotification;
            this.typingHandlers.forEach((h) => h(payload));
          } catch {
            // ignore malformed frames
          }
        });

        this._setState("connected");
      },

      // onDisconnect fires only on explicit deactivate(), not on WS drops.
      // STOMP.js reconnects automatically after WS drops via reconnectDelay.
      onDisconnect: () => {
        if (this.state !== "idle") {
          this._setState("idle");
        }
      },

      onStompError: () => {
        this._setState("error");
      },

      onWebSocketError: () => {
        // WS dropped — STOMP.js will retry after reconnectDelay.
        // Notify UI that we're temporarily disconnected.
        this._setState("error");
      }
    });

    this.client.activate();
  }

  // ── Disconnect ───────────────────────────────────────────────────────────

  disconnect(): void {
    this._internalDisconnect(/* silent */ false);
  }

  /** @param silent – true: don't fire connectionHandlers (used on reconnect) */
  private _internalDisconnect(silent: boolean): void {
    if (this.client?.active) {
      void this.client.deactivate();
    }
    this.client = null;
    this.currentRoomId = null;

    if (!silent && this.state !== "idle") {
      this._setState("idle");
    } else {
      this.state = "idle";
    }
  }

  private _setState(next: ChatConnectionState): void {
    this.state = next;
    const connected = next === "connected";
    // Only notify on actionable states (not "connecting")
    if (next !== "connecting") {
      this.connectionHandlers.forEach((h) => h(connected));
    }
  }

  // ── Send ─────────────────────────────────────────────────────────────────

  sendMessage(
    roomId: string,
    senderRole: string,
    senderName: string,
    receiverRole: string,
    receiverName: string,
    content: string
  ): boolean {
    if (!this.isConnected()) {
      return false;
    }

    const payload: ChatMessage = {
      roomId,
      senderRole,
      senderName,
      receiverRole,
      receiverName,
      content,
      timestamp: new Date().toISOString()
    };

    this.client!.publish({
      destination: `/app/chat.send/${roomId}`,
      body: JSON.stringify(payload)
    });

    return true;
  }

  sendTyping(
    roomId: string,
    senderRole: string,
    senderName: string,
    isTyping: boolean
  ): void {
    if (!this.isConnected()) {
      return;
    }

    this.client!.publish({
      destination: `/app/chat.typing/${roomId}`,
      body: JSON.stringify({ senderRole, senderName, isTyping })
    });
  }

  // ── Event subscriptions ──────────────────────────────────────────────────

  onMessage(handler: (msg: ChatMessage) => void): () => void {
    this.messageHandlers.add(handler);
    return () => this.messageHandlers.delete(handler);
  }

  onTyping(handler: (msg: TypingNotification) => void): () => void {
    this.typingHandlers.add(handler);
    return () => this.typingHandlers.delete(handler);
  }

  onConnection(handler: (connected: boolean) => void): () => void {
    this.connectionHandlers.add(handler);
    return () => this.connectionHandlers.delete(handler);
  }
}
