import type { SignalMessage } from "$lib/api/types";

const API_URL = (import.meta.env.VITE_API_URL as string | undefined)?.replace(/\/$/, "")
  ?? "https://signaling-server-tgsj.onrender.com";

const WS_URL = (import.meta.env.VITE_WS_URL as string | undefined)?.replace(/\/$/, "")
  ?? API_URL.replace(/^http:/i, "ws:").replace(/^https:/i, "wss:");

export class SignalingClient {
  private socket: WebSocket | null = null;
  private currentSessionId: string | null = null;
  private connectPromise: Promise<void> | null = null;
  private lastCloseEvent: CloseEvent | null = null;

  connect(signalingToken: string, role: "viewer" | "agent", sessionId?: string): Promise<void> {
    if (this.socket?.readyState === WebSocket.OPEN) {
      return Promise.resolve();
    }
    if (this.socket?.readyState === WebSocket.CONNECTING && this.connectPromise) {
      return this.connectPromise;
    }

    if (this.socket && this.socket.readyState !== WebSocket.CLOSED) {
      try {
        this.socket.close();
      } catch {
        // ignore stale socket close errors
      }
      this.socket = null;
    }

    this.connectPromise = new Promise((resolve, reject) => {
      this.currentSessionId = sessionId ?? null;
      const sessionParam = sessionId ? `&sessionId=${encodeURIComponent(sessionId)}` : "";
      const wsUrl = `${WS_URL}/ws/signaling?token=${encodeURIComponent(signalingToken)}&role=${role}${sessionParam}`;

      this.socket = new WebSocket(wsUrl);
      this.socket.onopen = () => {
        this.connectPromise = null;
        this.lastCloseEvent = null;
        resolve();
      };
      this.socket.onerror = () => {
        this.connectPromise = null;
        reject(new Error("WebSocket connection failed"));
      };
      this.socket.onclose = (event: CloseEvent) => {
        this.lastCloseEvent = event;
        this.socket = null;
        this.connectPromise = null;
      };
    });

    return this.connectPromise;
  }

  disconnect() {
    this.socket?.close();
    this.socket = null;
    this.connectPromise = null;
  }

  getLastCloseEvent() {
    return this.lastCloseEvent;
  }

  isConnected() {
    return this.socket?.readyState === WebSocket.OPEN;
  }

  send(message: SignalMessage, from: "viewer" | "agent" = "viewer") {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) {
      throw new Error("WebSocket not connected");
    }

    const payload: SignalMessage = {
      ...message,
      from,
      sessionId: message.sessionId ?? this.currentSessionId ?? undefined
    };

    this.socket.send(JSON.stringify(payload));
  }

  onMessage(handler: (message: SignalMessage) => void) {
    if (!this.socket) {
      return () => {};
    }

    const listener = (event: MessageEvent<string>) => {
      try {
        handler(JSON.parse(event.data) as SignalMessage);
      } catch {
        // Ignore malformed messages
      }
    };

    this.socket.addEventListener("message", listener);
    return () => this.socket?.removeEventListener("message", listener);
  }

  onClose(handler: (event: CloseEvent) => void) {
    if (!this.socket) {
      return () => {};
    }

    this.socket.addEventListener("close", handler);
    return () => this.socket?.removeEventListener("close", handler);
  }

  onError(handler: (event: Event) => void) {
    if (!this.socket) {
      return () => {};
    }

    this.socket.addEventListener("error", handler);
    return () => this.socket?.removeEventListener("error", handler);
  }
}
