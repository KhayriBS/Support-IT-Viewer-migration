import { ChatRealtimeClient, technicianApi } from "$lib/api";
import type { ChatMessage, ControlSession, TypingNotification } from "$lib/api";
import { mergeMessages, msgKey } from "$lib/utils/chat";
import { aiPipeline } from "./ai-pipeline.svelte";

class ChatManager {
  client = new ChatRealtimeClient();

  chatConnected = $state(false);
  chatRoomId = $state("");
  chatInput = $state("");
  chatMessages = $state<ChatMessage[]>([]);
  chatError = $state<string | null>(null);
  typingInfo = $state<TypingNotification | null>(null);
  chatListEl = $state<HTMLDivElement | null>(null);

  private typingClearTimer: ReturnType<typeof setTimeout> | null = null;
  private detachMessageListener: (() => void) | null = null;
  private detachTypingListener: (() => void) | null = null;
  private detachConnectionListener: (() => void) | null = null;
  private chatTypingDispatchTimer: ReturnType<typeof setTimeout> | null = null;
  private chatPollTimer: ReturnType<typeof setInterval> | null = null;

  // ── Callbacks set by the orchestrator ────────────────────────────────────
  getSession: () => ControlSession | null = () => null;
  getSelectedFeature: () => "screen" | "chat" | "files" | null = () => null;
  /** Returns true when the local agent is the target of the active session. */
  isLocalAgentTargeted: (session: ControlSession) => boolean = () => false;

  // ── Derived role helpers ─────────────────────────────────────────────────
  /**
   * Local user role. "agent" when the bridge says the local Tauri agent is
   * the target machine; "viewer" otherwise. Used so chat messages from the
   * agent side carry the right sender label.
   */
  chatLocalRole = $derived.by<"agent" | "viewer">(() => {
    const session = this.getSession();
    return session && this.isLocalAgentTargeted(session) ? "agent" : "viewer";
  });

  chatRemoteRole = $derived(this.chatLocalRole === "agent" ? "viewer" : "agent");

  // ── Helpers ──────────────────────────────────────────────────────────────
  private clearListeners = () => {
    this.detachMessageListener?.();
    this.detachTypingListener?.();
    this.detachConnectionListener?.();
    this.detachMessageListener = null;
    this.detachTypingListener = null;
    this.detachConnectionListener = null;
  };

  resolveRoomId = (): string => String(this.getSession()?.id ?? "").trim();

  private startPoll = () => {
    if (this.chatPollTimer) return;
    this.chatPollTimer = setInterval(() => {
      void this.refresh();
    }, 1500);
  };

  private stopPoll = () => {
    if (this.chatPollTimer) {
      clearInterval(this.chatPollTimer);
      this.chatPollTimer = null;
    }
  };

  // ── Core API ─────────────────────────────────────────────────────────────
  refresh = async (roomOverride?: string, replace = false) => {
    const roomId = roomOverride || this.chatRoomId || this.resolveRoomId();
    if (!roomId) return;

    try {
      const fetched = await technicianApi.getMessages(roomId);
      // Ignore stale responses when the user has switched to another session/room.
      if (this.chatRoomId && this.chatRoomId !== roomId) {
        return;
      }
      this.chatMessages = replace
        ? mergeMessages([], fetched)
        : mergeMessages(this.chatMessages, fetched);
      this.chatError = null;
    } catch (error) {
      this.chatError = String(error);
    }
  };

  connect = async () => {
    const roomId = this.resolveRoomId();
    if (!roomId) {
      this.chatError = "Aucune session active pour connecter le chat.";
      return;
    }

    if (this.chatRoomId === roomId && this.chatConnected) {
      return;
    }

    this.disconnect();
    this.chatMessages = [];
    this.typingInfo = null;
    this.chatRoomId = roomId;
    this.chatError = null;

    this.detachMessageListener = this.client.onMessage((msg) => {
      const k = msgKey(msg);
      if (this.chatMessages.some((m) => msgKey(m) === k)) return;
      this.chatMessages = [...this.chatMessages, msg].slice(-200);
    });

    this.detachTypingListener = this.client.onTyping((msg) => {
      this.typingInfo = msg.isTyping ? msg : null;
      if (this.typingClearTimer) clearTimeout(this.typingClearTimer);
      if (msg.isTyping) {
        this.typingClearTimer = setTimeout(() => { this.typingInfo = null; }, 3000);
      }
    });

    this.detachConnectionListener = this.client.onConnection((connected) => {
      this.chatConnected = connected;
      if (connected) {
        this.stopPoll();
      } else if (this.chatRoomId) {
        this.startPoll();
      }
    });

    await this.refresh(roomId, true);

    try {
      await this.client.connect(roomId);
    } catch (error) {
      this.chatError = String(error);
    }

    this.startPoll();

    // AI pipeline shares the same chat lifecycle (same STOMP /ws/chat base).
    aiPipeline.connect();
  };

  disconnect = () => {
    this.chatRoomId = "";
    this.client.disconnect();
    this.chatConnected = false;
    this.stopPoll();
    if (this.typingClearTimer) {
      clearTimeout(this.typingClearTimer);
      this.typingClearTimer = null;
    }
    this.typingInfo = null;
    this.clearListeners();
    aiPipeline.disconnect();
  };

  send = async () => {
    const roomId = this.chatRoomId || this.resolveRoomId();
    const content = this.chatInput.trim();
    if (!roomId || !content) return;

    // "/ai <prompt>" → capture frame + send to AI pipeline. The message is
    // NOT forwarded to the peer (avoid polluting the human chat).
    if (content.toLowerCase().startsWith("/ai ")) {
      if (this.getSelectedFeature() !== "screen") {
        this.chatError = "L'IA n'est disponible que depuis le panneau Écran (clique sur Écran pour démarrer le partage).";
        return;
      }
      const aiPrompt = content.slice(4).trim();
      if (!aiPrompt) {
        this.chatError = "Usage: /ai <commande>";
        return;
      }
      this.chatInput = "";
      await aiPipeline.sendAiCommand(aiPrompt);
      return;
    }

    const session = this.getSession();
    if (!session || session.status !== "ACTIVE") {
      this.chatError = "Aucune session active.";
      return;
    }

    const senderRole = this.chatLocalRole;
    const receiverRole = this.chatRemoteRole;

    const sentViaStomp = this.client.sendMessage(
      roomId,
      senderRole,
      senderRole,
      receiverRole,
      receiverRole,
      content
    );

    if (!sentViaStomp) {
      try {
        await technicianApi.sendMessageRest(
          roomId,
          senderRole,
          senderRole,
          receiverRole,
          receiverRole,
          content
        );
        await this.refresh();
      } catch (error) {
        this.chatError = String(error);
        return;
      }
    }

    this.chatInput = "";
    this.chatError = null;
  };

  dispatchTyping = () => {
    const roomId = this.chatRoomId || this.resolveRoomId();
    if (!roomId) return;
    this.client.sendTyping(roomId, this.chatLocalRole, this.chatLocalRole, true);
    if (this.chatTypingDispatchTimer) clearTimeout(this.chatTypingDispatchTimer);
    this.chatTypingDispatchTimer = setTimeout(() => {
      this.client.sendTyping(roomId, this.chatLocalRole, this.chatLocalRole, false);
      this.chatTypingDispatchTimer = null;
    }, 1500);
  };
}

export const chatManager = new ChatManager();
