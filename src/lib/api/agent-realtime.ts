import { Client, type IMessage } from "@stomp/stompjs";
import { technicianApi } from "$lib/api/technician-api";
import type { Agent } from "$lib/api/types";

function toWsBaseUrl(httpBase: string) {
  return httpBase.replace(/^http:/i, "ws:").replace(/^https:/i, "wss:").replace(/\/$/, "");
}

// Singleton STOMP client partagé. Lazy : on connecte au 1er subscribe et on
// déconnecte quand plus aucun handler n'écoute. Pas de room/typing, juste un
// broadcast global sur /topic/agents.
let sharedClient: Client | null = null;
const handlers = new Set<(a: Agent) => void>();
let stompSubscription: { unsubscribe: () => void } | null = null;

async function ensureClient() {
  if (sharedClient && sharedClient.active) return;

  const baseUrl = technicianApi.baseUrl.replace(/\/$/, "");
  const wsUrl = `${toWsBaseUrl(baseUrl)}/ws/chat`;
  const httpUrl = `${baseUrl}/ws/chat`;

  let sockjsCtor: (new (url: string) => WebSocket) | null = null;
  try {
    const mod = await import("sockjs-client");
    sockjsCtor =
      (mod as { default?: new (url: string) => WebSocket }).default
        ?? (mod as unknown as new (url: string) => WebSocket);
  } catch {
    /* fallback native WS */
  }

  sharedClient = new Client({
    reconnectDelay: 5000,
    heartbeatIncoming: 4000,
    heartbeatOutgoing: 4000,
    webSocketFactory: () => {
      if (sockjsCtor) {
        try { return new sockjsCtor(httpUrl); } catch { /* fallthrough */ }
      }
      return new WebSocket(wsUrl);
    },
    onConnect: () => {
      stompSubscription = sharedClient!.subscribe("/topic/agents", (msg: IMessage) => {
        try {
          const agent: Agent = JSON.parse(msg.body);
          handlers.forEach((h) => h(agent));
        } catch (e) {
          console.warn("[agent-realtime] payload non parseable", e);
        }
      });
    },
    onWebSocketClose: () => {
      stompSubscription = null;
    }
  });

  sharedClient.activate();
}

function teardownIfIdle() {
  if (handlers.size > 0) return;
  try { stompSubscription?.unsubscribe(); } catch { /* ignore */ }
  stompSubscription = null;
  sharedClient?.deactivate();
  sharedClient = null;
}

/** S'abonne aux mises à jour broadcastées d'agents. Retourne un unsubscribe. */
export function onAgentUpdate(handler: (a: Agent) => void): () => void {
  handlers.add(handler);
  void ensureClient();
  return () => {
    handlers.delete(handler);
    teardownIfIdle();
  };
}
