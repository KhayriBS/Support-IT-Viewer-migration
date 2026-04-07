import { Client, type IMessage } from "@stomp/stompjs";
import { technicianApi } from "$lib/api/technician-api";
import type { ChatMessage, TypingNotification } from "$lib/api/types";

function toWsBaseUrl(httpBase: string) {
  return httpBase.replace(/^http:/i, "ws:").replace(/^https:/i, "wss:").replace(/\/$/, "");
}

export class ChatRealtimeClient {
  private client: Client | null = null;
  private currentRoomId: string | null = null;
  private messageHandlers = new Set<(msg: ChatMessage) => void>();
  private typingHandlers = new Set<(msg: TypingNotification) => void>();
  private connectionHandlers = new Set<(connected: boolean) => void>();

  async connect(roomId: string) {
    if (this.client?.active && this.currentRoomId === roomId) {
      return;
    }

    this.disconnect();
    this.currentRoomId = roomId;

    const baseUrl = technicianApi.baseUrl.replace(/\/$/, "");
    const wsUrl = `${toWsBaseUrl(baseUrl)}/ws/chat`;
    const httpUrl = `${baseUrl}/ws/chat`;

    this.client = new Client({
      reconnectDelay: 5000,
      heartbeatIncoming: 4000,
      heartbeatOutgoing: 4000,
      debug: () => {},
      webSocketFactory: () => new WebSocket(wsUrl),
      onConnect: () => {
        this.connectionHandlers.forEach((h) => h(true));

        this.client?.subscribe(`/topic/chat/${roomId}`, (frame: IMessage) => {
          try {
            const payload = JSON.parse(frame.body) as ChatMessage;
            this.messageHandlers.forEach((h) => h(payload));
          } catch {
            // Ignore malformed payloads
          }
        });

        this.client?.subscribe(`/topic/chat/${roomId}/typing`, (frame: IMessage) => {
          try {
            const payload = JSON.parse(frame.body) as TypingNotification;
            this.typingHandlers.forEach((h) => h(payload));
          } catch {
            // Ignore malformed payloads
          }
        });
      },
      onDisconnect: () => {
        this.connectionHandlers.forEach((h) => h(false));
      },
      onStompError: () => {
        this.connectionHandlers.forEach((h) => h(false));
      },
      onWebSocketError: () => {
        this.connectionHandlers.forEach((h) => h(false));
      }
    });

    try {
      const mod = await import("sockjs-client");
      const SockJS = (mod as { default?: typeof WebSocket }).default as unknown as new (url: string) => WebSocket;
      this.client.webSocketFactory = () => {
        try {
          return new SockJS(httpUrl);
        } catch {
          return new WebSocket(wsUrl);
        }
      };
    } catch {
      // SockJS unavailable; keep WebSocket fallback.
    }

    this.client.activate();
  }

  disconnect() {
    if (this.client?.active) {
      void this.client.deactivate();
    }
    this.client = null;
    this.currentRoomId = null;
    this.connectionHandlers.forEach((h) => h(false));
  }

  isConnected() {
    return !!this.client?.connected;
  }

  sendMessage(
    roomId: string,
    senderRole: string,
    senderName: string,
    receiverRole: string,
    receiverName: string,
    content: string
  ) {
    if (!this.client?.connected) {
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

    this.client.publish({
      destination: `/app/chat.send/${roomId}`,
      body: JSON.stringify(payload)
    });

    return true;
  }

  sendTyping(roomId: string, senderRole: string, senderName: string, isTyping: boolean) {
    if (!this.client?.connected) {
      return;
    }

    this.client.publish({
      destination: `/app/chat.typing/${roomId}`,
      body: JSON.stringify({ senderRole, senderName, isTyping })
    });
  }

  onMessage(handler: (msg: ChatMessage) => void) {
    this.messageHandlers.add(handler);
    return () => this.messageHandlers.delete(handler);
  }

  onTyping(handler: (msg: TypingNotification) => void) {
    this.typingHandlers.add(handler);
    return () => this.typingHandlers.delete(handler);
  }

  onConnection(handler: (connected: boolean) => void) {
    this.connectionHandlers.add(handler);
    return () => this.connectionHandlers.delete(handler);
  }
}
