import type { SignalMessage } from "$lib/api/types";

const API_URL = (import.meta.env.VITE_API_URL as string | undefined)?.replace(/\/$/, "")
  ?? "http://192.168.218.49:8080";

const WS_URL = (import.meta.env.VITE_WS_URL as string | undefined)?.replace(/\/$/, "")
  ?? API_URL.replace(/^http:/i, "ws:").replace(/^https:/i, "wss:");

export class SignalingClient {
  private socket: WebSocket | null = null;
  private currentSessionId: string | null = null;

  connect(signalingToken: string, role: "viewer" | "agent", sessionId?: string): Promise<void> {
    return new Promise((resolve, reject) => {
      if (this.socket?.readyState === WebSocket.OPEN) {
        resolve();
        return;
      }

      this.currentSessionId = sessionId ?? null;
      const sessionParam = sessionId ? `&sessionId=${encodeURIComponent(sessionId)}` : "";
      const wsUrl = `${WS_URL}/ws/signaling?token=${encodeURIComponent(signalingToken)}&role=${role}${sessionParam}`;

      this.socket = new WebSocket(wsUrl);
      this.socket.onopen = () => resolve();
      this.socket.onerror = () => reject(new Error("WebSocket connection failed"));
      this.socket.onclose = () => {
        this.socket = null;
      };
    });
  }

  disconnect() {
    this.socket?.close();
    this.socket = null;
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

  onClose(handler: () => void) {
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
