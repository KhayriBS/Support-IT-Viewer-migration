<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import { ChatRealtimeClient, SignalingClient, technicianApi } from "$lib/api";
  import type { Agent, ChatMessage, ControlSession, SignalMessage, TypingNotification } from "$lib/api";

  interface AgentMetrics {
    cpuUsage: number;
    ramUsage: number;
    diskUsage: number;
    timestamp: number;
  }

  let metrics = $state<AgentMetrics | null>(null);
  let metricsError = $state<string | null>(null);
  let metricsLoading = $state(true);
  let agentRunning = $state(false);
  let agentLifecycleError = $state<string | null>(null);

  let onlineAgents = $state<Agent[]>([]);
  let agentsLoading = $state(false);
  let agentsError = $state<string | null>(null);
  let agentsUpdatedAt = $state<string>("-");

  let activeSession = $state<ControlSession | null>(null);
  let actionLoading = $state(false);
  let actionError = $state<string | null>(null);
  let waitingForApproval = $state(false);
  let selectedFeature = $state<"screen" | "chat" | "files" | null>(null);

  let connectionCode = $state("");
  let sessionTokenQuery = $state("");
  let queriedSession = $state<ControlSession | null>(null);

  interface SignalLogEntry {
    timestamp: string;
    direction: "in" | "out";
    type: string;
    from: string;
    to: string;
    payload: string;
  }

  const signalingClient = new SignalingClient();
  const chatClient = new ChatRealtimeClient();
  let signalingConnected = $state(false);
  let signalingError = $state<string | null>(null);
  let backendSessionSynced = $state(false);
  let backendSyncError = $state<string | null>(null);
  let signalLogs = $state<SignalLogEntry[]>([]);
  let outgoingChat = $state("Hello from lumiere-tech-it");
  let detachMessageListener: (() => void) | null = null;
  let detachCloseListener: (() => void) | null = null;
  let detachErrorListener: (() => void) | null = null;
  let viewerPeerConnection: RTCPeerConnection | null = null;
  let viewerVideoEl = $state<HTMLVideoElement | null>(null);
  let viewerRemoteStream = $state<MediaStream | null>(null);
  let screenFrameUrl = $state<string>("");
  let screenFrameAt = $state<string>("");
  let screenFrameCount = $state<number>(0);
  let screenFrameError = $state<string | null>(null);

  let metricsTimer: ReturnType<typeof setInterval>;
  let agentsTimer: ReturnType<typeof setInterval>;
  let chatPollTimer: ReturnType<typeof setInterval> | null = null;

  let chatConnected = $state(false);
  let chatRoomId = $state("");
  let chatInput = $state("");
  let chatMessages = $state<ChatMessage[]>([]);
  let chatError = $state<string | null>(null);
  let typingInfo = $state<TypingNotification | null>(null);
  let detachChatMessageListener: (() => void) | null = null;
  let detachChatTypingListener: (() => void) | null = null;
  let detachChatConnectionListener: (() => void) | null = null;

  // Session approval modal
  let machineId = $state<string>("");
  let localMachineId = $state<string>("");
  let showApprovalModal = $state(false);
  let pendingApprovalSession = $state<ControlSession | null>(null);
  let approvalAllowRemoteInput = $state(true);
  let approvalAllowFileTransfer = $state(true);
  let approvalLoading = $state(false);
  let approvalError = $state<string | null>(null);
  let approvalTimer: ReturnType<typeof setInterval> | null = null;
  let sessionActivationTimer: ReturnType<typeof setInterval> | null = null;

  interface AgentStatusSnapshot {
    running: boolean;
    machineId: string;
  }

  async function refreshMetrics() {
    try {
      const payload = await invoke<AgentMetrics>("get_metrics");
      metrics = payload;
      metricsError = null;
    } catch (error) {
      metricsError = String(error);
    } finally {
      metricsLoading = false;
    }
  }

  async function syncAgentLifecycle() {
    try {
      let status = await invoke<AgentStatusSnapshot>("get_agent_status");
      agentRunning = status.running;

      if (!status.running) {
        await invoke("start_agent_cmd", { serverUrl: technicianApi.baseUrl });
        status = await invoke<AgentStatusSnapshot>("get_agent_status");
        agentRunning = status.running;
      }

      localMachineId = status.machineId?.trim() ?? "";

      agentLifecycleError = null;
    } catch (error) {
      agentLifecycleError = String(error);
      agentRunning = false;
    }
  }

  async function stopAgentLifecycle() {
    try {
      await invoke("stop_agent_cmd");
    } catch {
      // ignore shutdown errors
    } finally {
      agentRunning = false;
    }
  }

  async function refreshOnlineAgents() {
    agentsLoading = true;
    try {
      onlineAgents = await technicianApi.getOnlineAgents();
      agentsError = null;
      agentsUpdatedAt = new Date().toLocaleTimeString();
    } catch (error) {
      agentsError = String(error);
    } finally {
      agentsLoading = false;
    }
  }

  async function startSession(machineId: string) {
    actionLoading = true;
    actionError = null;
    waitingForApproval = false;
    selectedFeature = null;
    try {
      activeSession = await technicianApi.startSession(machineId);
      queriedSession = activeSession;
      sessionTokenQuery = activeSession.signalingToken;
      waitingForApproval = activeSession.status === "PENDING_APPROVAL";
      watchSessionActivation(activeSession.signalingToken);
    } catch (error) {
      actionError = String(error);
      waitingForApproval = false;
    } finally {
      actionLoading = false;
    }
  }

  async function startSessionWithCode() {
    const code = connectionCode.trim();
    if (!code) {
      actionError = "Veuillez renseigner un code de connexion.";
      return;
    }

    actionLoading = true;
    actionError = null;
    waitingForApproval = false;
    selectedFeature = null;
    try {
      activeSession = await technicianApi.startSessionByCode(code);
      queriedSession = activeSession;
      sessionTokenQuery = activeSession.signalingToken;
      waitingForApproval = activeSession.status === "PENDING_APPROVAL";
      watchSessionActivation(activeSession.signalingToken);
    } catch (error) {
      actionError = String(error);
      waitingForApproval = false;
    } finally {
      actionLoading = false;
    }
  }

  function stopSessionActivationWatch() {
    if (sessionActivationTimer) {
      clearInterval(sessionActivationTimer);
      sessionActivationTimer = null;
    }
  }

  function watchSessionActivation(sessionToken: string) {
    stopSessionActivationWatch();
    if (!sessionToken) {
      return;
    }

    let attempts = 0;
    let inFlight = false;

    sessionActivationTimer = setInterval(async () => {
      if (inFlight || signalingConnected) {
        return;
      }

      inFlight = true;
      attempts += 1;

      try {
        const session = await technicianApi.getSessionByToken(sessionToken);
        if (!session) {
          if (attempts >= 90) {
            stopSessionActivationWatch();
          }
          return;
        }

        queriedSession = session;
        activeSession = session;

        if (session.status === "ACTIVE") {
          waitingForApproval = false;
          if (!selectedFeature) {
            selectedFeature = "screen";
          }
          stopSessionActivationWatch();
          await connectSignaling();
          return;
        }

        if (session.status === "TERMINATED" || attempts >= 90) {
          waitingForApproval = false;
          stopSessionActivationWatch();
        }
      } catch {
        if (attempts >= 90) {
          waitingForApproval = false;
          stopSessionActivationWatch();
        }
      } finally {
        inFlight = false;
      }
    }, 2000);
  }

  async function stopByToken() {
    const token = (activeSession?.signalingToken ?? sessionTokenQuery).trim();
    if (!token) {
      actionError = "Aucun token de session a arreter.";
      return;
    }

    actionLoading = true;
    actionError = null;
    try {
      stopSessionActivationWatch();
      await disconnectSignaling();
      await technicianApi.stopSessionByToken(token);
      activeSession = null;
      queriedSession = null;
      waitingForApproval = false;
      selectedFeature = null;
    } catch (error) {
      actionError = String(error);
    } finally {
      actionLoading = false;
    }
  }

  function chooseFeature(feature: "screen" | "chat" | "files") {
    selectedFeature = feature;
    if (feature === "chat") {
      void connectChat();
    }
  }

  async function lookupSession() {
    const token = sessionTokenQuery.trim();
    if (!token) {
      actionError = "Veuillez renseigner un token de session.";
      return;
    }

    actionLoading = true;
    actionError = null;
    try {
      queriedSession = await technicianApi.getSessionByToken(token);
    } catch (error) {
      actionError = String(error);
    } finally {
      actionLoading = false;
    }
  }

  function clearChatListeners() {
    detachChatMessageListener?.();
    detachChatTypingListener?.();
    detachChatConnectionListener?.();
    detachChatMessageListener = null;
    detachChatTypingListener = null;
    detachChatConnectionListener = null;
  }

  function resolveRoomId() {
    return String((queriedSession ?? activeSession)?.id ?? "").trim();
  }

  async function refreshChatMessages() {
    const roomId = chatRoomId || resolveRoomId();
    if (!roomId) {
      return;
    }

    try {
      chatMessages = await technicianApi.getMessages(roomId);
      chatError = null;
    } catch (error) {
      chatError = String(error);
    }
  }

  async function connectChat() {
    const roomId = resolveRoomId();
    if (!roomId) {
      chatError = "Aucune session active pour connecter le chat.";
      return;
    }

    disconnectChat();
    chatRoomId = roomId;
    chatError = null;

    detachChatMessageListener = chatClient.onMessage((msg) => {
      chatMessages = [...chatMessages, msg].slice(-100);
    });

    detachChatTypingListener = chatClient.onTyping((msg) => {
      typingInfo = msg;
      setTimeout(() => {
        if (typingInfo === msg) {
          typingInfo = null;
        }
      }, 1200);
    });

    detachChatConnectionListener = chatClient.onConnection((connected) => {
      chatConnected = connected;
    });

    await refreshChatMessages();

    try {
      await chatClient.connect(roomId);
    } catch (error) {
      chatError = String(error);
    }

    if (chatPollTimer) {
      clearInterval(chatPollTimer);
    }
    // Fallback refresh even if STOMP drops.
    chatPollTimer = setInterval(() => {
      void refreshChatMessages();
    }, 5000);
  }

  function disconnectChat() {
    chatClient.disconnect();
    chatConnected = false;
    if (chatPollTimer) {
      clearInterval(chatPollTimer);
      chatPollTimer = null;
    }
    clearChatListeners();
  }

  async function sendChatMessage() {
    const roomId = chatRoomId || resolveRoomId();
    const content = chatInput.trim();
    if (!roomId || !content) {
      return;
    }

    const sentViaStomp = chatClient.sendMessage(
      roomId,
      "viewer",
      "viewer",
      "agent",
      "agent",
      content
    );

    if (!sentViaStomp) {
      try {
        await technicianApi.sendMessageRest(roomId, "viewer", "viewer", "agent", "agent", content);
        await refreshChatMessages();
      } catch (error) {
        chatError = String(error);
        return;
      }
    }

    chatInput = "";
    chatError = null;
  }

  function statusClass(status: string | undefined) {
    switch ((status ?? "").toUpperCase()) {
      case "ONLINE":
        return "ok";
      case "BUSY":
        return "warn";
      default:
        return "muted";
    }
  }

  function logSignal(direction: "in" | "out", msg: SignalMessage) {
    const payloadText = msg.payload === undefined ? "" : JSON.stringify(msg.payload);
    const next: SignalLogEntry = {
      timestamp: new Date().toLocaleTimeString(),
      direction,
      type: msg.type,
      from: msg.from ?? "",
      to: msg.to,
      payload: payloadText
    };

    signalLogs = [next, ...signalLogs].slice(0, 30);
  }

  function clearSignalingListeners() {
    detachMessageListener?.();
    detachCloseListener?.();
    detachErrorListener?.();
    detachMessageListener = null;
    detachCloseListener = null;
    detachErrorListener = null;
  }

  function resetViewerPeerConnection() {
    try {
      viewerPeerConnection?.close();
    } catch {
      // ignore close errors
    } finally {
      viewerPeerConnection = null;
    }

    if (viewerVideoEl) {
      try {
        viewerVideoEl.srcObject = null;
      } catch {
        // ignore
      }
    }

    try {
      viewerRemoteStream?.getTracks().forEach((track) => track.stop());
    } catch {
      // ignore
    }
    viewerRemoteStream = null;

    screenFrameUrl = "";
    screenFrameAt = "";
    screenFrameCount = 0;
  }

  function ensureViewerPeerConnection(sessionId: string) {
    if (viewerPeerConnection) {
      return viewerPeerConnection;
    }

    const pc = new RTCPeerConnection({
      iceServers: [{ urls: "stun:stun.l.google.com:19302" }]
    });

    // Needed to produce an SDP offer even before media integration is complete.
    pc.createDataChannel("control");
    pc.addTransceiver("video", { direction: "recvonly" });

    pc.ontrack = (event) => {
      const stream = event.streams?.[0] ?? new MediaStream([event.track]);
      viewerRemoteStream = stream;
      if (viewerVideoEl && viewerVideoEl.srcObject !== stream) {
        viewerVideoEl.srcObject = stream;
        void viewerVideoEl.play().catch(() => {
          // Autoplay may be blocked until user gesture.
        });
      }
    };

    pc.onicecandidate = (event) => {
      if (!event.candidate || !signalingConnected) {
        return;
      }

      const iceMessage: SignalMessage = {
        type: "ICE",
        to: "agent",
        sessionId,
        payload: {
          candidate: event.candidate.candidate,
          sdpMid: event.candidate.sdpMid,
          sdpMLineIndex: event.candidate.sdpMLineIndex
        }
      };

      try {
        signalingClient.send(iceMessage, "viewer");
        logSignal("out", { ...iceMessage, from: "viewer" });
      } catch {
        // ignore transient send issues
      }
    };

    viewerPeerConnection = pc;
    return pc;
  }

  async function sendViewerOffer(sessionId: string) {
    const pc = ensureViewerPeerConnection(sessionId);
    const offer = await pc.createOffer({ offerToReceiveVideo: true });
    await pc.setLocalDescription(offer);

    const offerMessage: SignalMessage = {
      type: "OFFER",
      to: "agent",
      sessionId,
      payload: {
        type: offer.type,
        sdp: offer.sdp
      }
    };

    signalingClient.send(offerMessage, "viewer");
    logSignal("out", { ...offerMessage, from: "viewer" });
  }

  async function handleIncomingSignal(message: SignalMessage) {
    if (message.type === "FILE_DATA" || message.type === "STREAM_STATS") {
      const payload = message.payload as unknown;

      const extractFrame = (): { mime: string; data: string; timestamp?: string } | null => {
        if (typeof payload === "string") {
          return payload.length > 100 ? { mime: "image/jpeg", data: payload } : null;
        }

        if (!payload || typeof payload !== "object") {
          return null;
        }

        const record = payload as Record<string, unknown>;

        // Ignore actual file-transfer chunks.
        if (
          typeof record.chunkIndex === "number" ||
          typeof record.totalChunks === "number" ||
          typeof record.fileName === "string"
        ) {
          return null;
        }

        const kind = typeof record.kind === "string" ? record.kind : "";
        const data = record.data ?? record.base64 ?? record.image ?? record.frame;
        const mime = (record.mime ?? record.contentType ?? "image/jpeg") as string;
        const timestamp = typeof record.timestamp === "string" ? record.timestamp : undefined;

        const isScreenFrame = kind === "screen-frame";
        const isImageMime = typeof mime === "string" && mime.startsWith("image/");
        const looksLikeFrame = "frame" in record || "image" in record;

        if (!isScreenFrame && !isImageMime && !looksLikeFrame) {
          return null;
        }

        if (typeof data === "string" && data.length > 100) {
          return { mime, data, timestamp };
        }

        return null;
      };

      const frame = extractFrame();
      if (frame) {
        screenFrameUrl = `data:${frame.mime};base64,${frame.data}`;
        screenFrameAt = frame.timestamp || new Date().toISOString();
        screenFrameCount += 1;
        screenFrameError = null;
      }
      return;
    }

    if (message.type === "ANSWER") {
      const payload = message.payload as { type?: string; sdp?: string } | null;
      if (!payload?.sdp || !payload?.type) {
        return;
      }

      const pc = viewerPeerConnection;
      if (!pc) {
        return;
      }

      await pc.setRemoteDescription({
        type: payload.type as RTCSdpType,
        sdp: payload.sdp
      });
      return;
    }

    if (message.type === "ICE") {
      const payload = message.payload as {
        candidate?: string;
        sdpMid?: string;
        sdpMLineIndex?: number;
      } | null;

      if (!payload?.candidate || !viewerPeerConnection) {
        return;
      }

      await viewerPeerConnection.addIceCandidate({
        candidate: payload.candidate,
        sdpMid: payload.sdpMid ?? null,
        sdpMLineIndex: payload.sdpMLineIndex ?? null
      });
    }
  }

  async function joinBackendSession(session: ControlSession) {
    await invoke("join_session_cmd", {
      signalingToken: session.signalingToken,
      sessionId: session.id,
      allowRemoteInput: session.allowRemoteInput ?? true,
      allowFileTransfer: session.allowFileTransfer ?? true,
      serverUrl: technicianApi.baseUrl
    });
    backendSessionSynced = true;
    backendSyncError = null;
  }

  async function leaveBackendSession() {
    try {
      await invoke("leave_session_cmd");
      backendSyncError = null;
    } catch (error) {
      backendSyncError = String(error);
    } finally {
      backendSessionSynced = false;
    }
  }

  async function connectSignaling() {
    const current = queriedSession ?? activeSession;
    if (!current) {
      signalingError = "Demarrez ou chargez une session avant la connexion signaling.";
      return;
    }

    signalingError = null;
    try {
      await signalingClient.connect(current.signalingToken, "viewer", String(current.id));
      try {
        await joinBackendSession(current);
      } catch (error) {
        backendSessionSynced = false;
        backendSyncError = String(error);
      }
      clearSignalingListeners();

      detachMessageListener = signalingClient.onMessage((message) => {
        logSignal("in", message);
        void handleIncomingSignal(message);
      });

      detachCloseListener = signalingClient.onClose(() => {
        signalingConnected = false;
        resetViewerPeerConnection();
        void leaveBackendSession();
      });

      detachErrorListener = signalingClient.onError(() => {
        signalingError = "Erreur socket signaling";
      });

      signalingConnected = true;

      const joinMessage: SignalMessage = {
        type: "JOIN",
        to: "agent",
        sessionId: String(current.id),
        payload: {
          role: "viewer"
        }
      };
      signalingClient.send(joinMessage, "viewer");
      logSignal("out", { ...joinMessage, from: "viewer" });
      await sendViewerOffer(String(current.id));
    } catch (error) {
      signalingClient.disconnect();
      resetViewerPeerConnection();
      signalingConnected = false;
      backendSessionSynced = false;
      backendSyncError = null;
      signalingError = String(error);
    }
  }

  async function disconnectSignaling() {
    signalingClient.disconnect();
    resetViewerPeerConnection();
    clearSignalingListeners();
    signalingConnected = false;
    await leaveBackendSession();
  }

  function sendChatSignal() {
    const text = outgoingChat.trim();
    if (!text) {
      return;
    }

    const msg: SignalMessage = {
      type: "CHAT",
      to: "agent",
      payload: {
        content: text,
        senderName: "viewer",
        timestamp: new Date().toISOString()
      }
    };

    try {
      signalingClient.send(msg, "viewer");
      logSignal("out", { ...msg, from: "viewer" });
      signalingError = null;
    } catch (error) {
      signalingError = String(error);
    }
  }

  async function loadLocalMachineId() {
    try {
      const status = await invoke<AgentStatusSnapshot>("get_agent_status");
      localMachineId = status.machineId?.trim() ?? "";
    } catch {
      localMachineId = "";
    }
  }

  async function checkPendingApproval() {
    if (!localMachineId) {
      await loadLocalMachineId();
      if (!localMachineId) {
        return;
      }
    }

    try {
      const session = await technicianApi.getPendingApprovalPublic(localMachineId);
      if (session && session.status === "PENDING_APPROVAL") {
        pendingApprovalSession = session;
        showApprovalModal = true;
        approvalAllowRemoteInput = true;
        approvalAllowFileTransfer = true;
      }
      approvalError = null;
    } catch (error) {
      // Silent fail for polling
    }
  }

  async function approvePendingSession() {
    if (!pendingApprovalSession || approvalLoading) return;

    approvalLoading = true;
    approvalError = null;

    try {
      await technicianApi.approveSessionPublic(
        pendingApprovalSession.id,
        approvalAllowRemoteInput,
        approvalAllowFileTransfer
      );
      approvalLoading = false;
      showApprovalModal = false;
      pendingApprovalSession = null;
    } catch (error) {
      approvalLoading = false;
      approvalError = String(error);
    }
  }

  async function rejectPendingSession() {
    if (!pendingApprovalSession || approvalLoading) return;

    approvalLoading = true;
    approvalError = null;

    try {
      await technicianApi.rejectSessionPublic(pendingApprovalSession.id);
      approvalLoading = false;
      showApprovalModal = false;
      pendingApprovalSession = null;
    } catch (error) {
      approvalLoading = false;
      approvalError = String(error);
    }
  }

  onMount(() => {
    void syncAgentLifecycle();
    void loadLocalMachineId();
    refreshMetrics();
    refreshOnlineAgents();
    void checkPendingApproval();

    metricsTimer = setInterval(refreshMetrics, 2500);
    agentsTimer = setInterval(refreshOnlineAgents, 8000);
    approvalTimer = setInterval(checkPendingApproval, 3000);
  });

  onDestroy(() => {
    clearInterval(metricsTimer);
    clearInterval(agentsTimer);
    if (approvalTimer) clearInterval(approvalTimer);
    stopSessionActivationWatch();
    void disconnectSignaling();
    disconnectChat();
    void stopAgentLifecycle();
  });
</script>

<svelte:head>
  <title>Lumiere IT | Dashboard</title>
  <meta name="description" content="Dashboard API migre depuis TechnicianViewer" />
</svelte:head>

<main>
  <header>
    <h1>Lumiere IT Dashboard</h1>
    <div class="badges">
      <span class="badge" class:ok={!metricsError && !metricsLoading} class:error={!!metricsError}>
        Local metrics: {metricsLoading ? "loading" : metricsError ? "error" : "ok"}
      </span>
      <span class="badge" class:ok={agentRunning} class:error={!agentRunning}>
        Agent: {agentRunning ? "online" : "offline"}
      </span>
      <span class="badge" class:ok={!agentsError && !agentsLoading} class:error={!!agentsError}>
        Backend API: {agentsLoading ? "loading" : agentsError ? "error" : "ok"}
      </span>
      <span class="badge" class:ok={signalingConnected} class:error={!signalingConnected}>
        Signaling WS: {signalingConnected ? "connected" : "disconnected"}
      </span>
      <span class="badge" class:ok={chatConnected} class:error={!chatConnected}>
        Chat STOMP: {chatConnected ? "connected" : "fallback"}
      </span>
      <span class="badge" class:ok={backendSessionSynced} class:error={!backendSessionSynced}>
        Tauri session: {backendSessionSynced ? "synced" : "idle"}
      </span>
    </div>
  </header>

  <section class="grid metrics">
    <article class="card">
      <h2>CPU</h2>
      <p class="big">{metrics ? `${metrics.cpuUsage.toFixed(1)}%` : "-"}</p>
    </article>
    <article class="card">
      <h2>RAM</h2>
      <p class="big">{metrics ? `${metrics.ramUsage.toFixed(1)}%` : "-"}</p>
    </article>
    <article class="card">
      <h2>Disk</h2>
      <p class="big">{metrics ? `${metrics.diskUsage.toFixed(1)}%` : "-"}</p>
    </article>
  </section>

  <section class="card">
    {#if agentLifecycleError}
      <p class="error top-gap">{agentLifecycleError}</p>
    {/if}

    <div class="row between">
      <h2>Agents en ligne</h2>
      <button onclick={refreshOnlineAgents} disabled={agentsLoading || actionLoading}>Rafraichir</button>
    </div>
    <p class="hint">Derniere synchro: {agentsUpdatedAt}</p>

    {#if agentsError}
      <p class="error">{agentsError}</p>
    {:else if onlineAgents.length === 0}
      <p class="hint">Aucun agent online.</p>
    {:else}
      <div class="list">
        {#each onlineAgents as agent (agent.id)}
          <div class="item">
            <div>
              <strong>{agent.machineId}</strong>
              <p>{agent.hostname} - {agent.osInfo}</p>
            </div>
            <div class="row">
              <span class={`pill ${statusClass(agent.status)}`}>{agent.status}</span>
              <button onclick={() => startSession(agent.machineId)} disabled={actionLoading}>Start session</button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <section class="grid actions">
    <article class="card">
      <h2>Start by code</h2>
      <div class="row">
        <input bind:value={connectionCode} placeholder="Connection code" />
        <button onclick={startSessionWithCode} disabled={actionLoading}>Start</button>
      </div>
    </article>

    <article class="card">
      <h2>Session by token</h2>
      <div class="row">
        <input bind:value={sessionTokenQuery} placeholder="Session token" />
        <button onclick={lookupSession} disabled={actionLoading}>Lookup</button>
      </div>
      <div class="row top-gap">
        <button class="danger" onclick={stopByToken} disabled={actionLoading}>Stop by token</button>
      </div>
    </article>
  </section>

  <section class="card">
    <h2>Session courante</h2>
    {#if queriedSession}
      <div class="session">
        <p><strong>ID:</strong> {queriedSession.id}</p>
        <p><strong>Machine:</strong> {queriedSession.agentMachineId}</p>
        <p><strong>Technicien:</strong> {queriedSession.technicianUsername}</p>
        <p><strong>Status:</strong> {queriedSession.status}</p>
        <p><strong>Token:</strong> <code>{queriedSession.signalingToken}</code></p>
      </div>

      {#if waitingForApproval || queriedSession.status === "PENDING_APPROVAL"}
        <p class="hint top-gap waiting-msg">
          Demande envoyee. En attente de confirmation sur le PC distant...
        </p>
      {/if}

      {#if queriedSession.status === "ACTIVE"}
        <div class="row top-gap feature-actions">
          <button
            class:selected={selectedFeature === "screen"}
            onclick={() => chooseFeature("screen")}
          >
            Screen
          </button>
          <button
            class:selected={selectedFeature === "chat"}
            onclick={() => chooseFeature("chat")}
          >
            Chat
          </button>
          <button
            class:selected={selectedFeature === "files"}
            onclick={() => chooseFeature("files")}
          >
            Transfert fichier
          </button>
        </div>
      {/if}
    {:else}
      <p class="hint">Aucune session chargee.</p>
    {/if}

    {#if actionError}
      <p class="error top-gap">{actionError}</p>
    {/if}
  </section>

  {#if queriedSession?.status === "ACTIVE" && selectedFeature === "screen"}
  <section class="card">
    <div class="row between">
      <h2>Screen (signaling video)</h2>
      <div class="row">
        <button onclick={connectSignaling} disabled={actionLoading || signalingConnected}>Connect</button>
        <button onclick={() => void disconnectSignaling()} disabled={!signalingConnected}>Disconnect</button>
      </div>
    </div>

    {#if signalingError}
      <p class="error top-gap">{signalingError}</p>
    {/if}

    {#if backendSyncError}
      <p class="error top-gap">{backendSyncError}</p>
    {/if}

    {#if signalLogs.length === 0}
      <p class="hint top-gap">Aucun signal recu pour le moment.</p>
    {:else}
      <div class="list top-gap">
        {#each signalLogs as log, i (`${log.timestamp}-${i}`)}
          <div class="item">
            <p class="mono">
              [{log.timestamp}] {log.direction.toUpperCase()} {log.type} {log.from} -&gt; {log.to}
            </p>
            <p class="hint mono">{log.payload || "(no payload)"}</p>
          </div>
        {/each}
      </div>
    {/if}

    <div class="top-gap screen-frame-panel">
      <div class="row between">
        <h3>Apercu ecran distant</h3>
        <span class="pill {viewerRemoteStream || screenFrameUrl ? 'ok' : 'muted'}">
          {viewerRemoteStream ? 'webrtc' : screenFrameUrl ? 'frame reçue' : 'en attente'}
        </span>
      </div>

      <div class="top-gap video-shell">
        {#if viewerRemoteStream}
          <video class="viewer-video" bind:this={viewerVideoEl} autoplay playsinline muted></video>
        {:else if screenFrameUrl}
          <img class="screen-preview" src={screenFrameUrl} alt="Remote screen preview" />
        {:else}
          <div class="video-placeholder">
            <p>Aucune image reçue pour le moment.</p>
            <p class="hint">Lance la session puis attends les frames envoyées par l’agent.</p>
          </div>
        {/if}
      </div>

      {#if !viewerRemoteStream && screenFrameUrl}
        <p class="hint mono">Frame #{screenFrameCount} · {screenFrameAt}</p>
      {/if}

      {#if screenFrameError}
        <p class="error top-gap">{screenFrameError}</p>
      {/if}
    </div>
  </section>
  {/if}

  {#if queriedSession?.status === "ACTIVE" && selectedFeature === "chat"}
  <section class="card">
    <div class="row between">
      <h2>Chat live (STOMP)</h2>
      <div class="row">
        <button onclick={connectChat} disabled={chatConnected}>Connect chat</button>
        <button onclick={disconnectChat} disabled={!chatConnected}>Disconnect chat</button>
      </div>
    </div>

    <div class="row top-gap">
      <input bind:value={chatInput} placeholder="Message" oninput={() => chatClient.sendTyping(chatRoomId || resolveRoomId(), "viewer", "viewer", true)} />
      <button onclick={sendChatMessage}>Send</button>
    </div>

    {#if typingInfo}
      <p class="hint top-gap">{typingInfo.senderName} is typing...</p>
    {/if}

    {#if chatError}
      <p class="error top-gap">{chatError}</p>
    {/if}

    {#if chatMessages.length === 0}
      <p class="hint top-gap">Aucun message.</p>
    {:else}
      <div class="list top-gap">
        {#each chatMessages as msg, i (`${msg.id ?? "x"}-${msg.timestamp}-${i}`)}
          <div class="item">
            <p><strong>{msg.senderName}</strong>: {msg.content}</p>
            <p class="hint mono">{msg.timestamp}</p>
          </div>
        {/each}
      </div>
    {/if}
  </section>
  {/if}

  {#if queriedSession?.status === "ACTIVE" && selectedFeature === "files"}
    <section class="card">
      <h2>Transfert de fichiers</h2>
      <p class="hint top-gap">
        Canal pret. La vue de navigation/transfert fichier sera branchee ici.
      </p>
    </section>
  {/if}

  <!-- Approval Modal -->
  {#if showApprovalModal && pendingApprovalSession}
    <div class="approval-overlay" role="dialog" tabindex="-1" onkeydown={(e) => e.key === "Escape" && !approvalLoading && (showApprovalModal = false)} onclick={() => !approvalLoading && (showApprovalModal = false)}>
      <div class="approval-modal" onclick={(e) => e.stopPropagation()}>
        <h2>Demande d'accès distant</h2>
        <p class="approval-desc">
          <strong>{pendingApprovalSession.technicianUsername || "Technicien"}</strong> demande l'accès à ce PC.
        </p>

        {#if approvalError}
          <p class="error top-gap">{approvalError}</p>
        {/if}

        <div class="approval-options">
          <label>
            <input type="checkbox" bind:checked={approvalAllowRemoteInput} disabled={approvalLoading} />
            Autoriser clavier / souris
          </label>
          <label>
            <input type="checkbox" bind:checked={approvalAllowFileTransfer} disabled={approvalLoading} />
            Autoriser transfert de fichiers
          </label>
        </div>

        <div class="approval-actions">
          <button 
            class="btn-reject" 
            onclick={rejectPendingSession} 
            disabled={approvalLoading}>
            {approvalLoading ? "Traitement..." : "Refuser"}
          </button>
          <button 
            class="btn-approve" 
            onclick={approvePendingSession} 
            disabled={approvalLoading}>
            {approvalLoading ? "Traitement..." : "Autoriser"}
          </button>
        </div>
      </div>
    </div>
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: "Segoe UI", sans-serif;
    background: linear-gradient(160deg, #0f172a, #111827 45%, #1e293b);
    color: #e2e8f0;
  }

  main {
    max-width: 1100px;
    margin: 0 auto;
    padding: 24px;
    display: grid;
    gap: 16px;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }

  h1, h2 {
    margin: 0;
    font-weight: 700;
  }

  .grid {
    display: grid;
    gap: 12px;
  }

  .metrics {
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
  }

  .actions {
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  }

  .card {
    background: rgba(15, 23, 42, 0.8);
    border: 1px solid rgba(148, 163, 184, 0.2);
    border-radius: 12px;
    padding: 14px;
    backdrop-filter: blur(6px);
  }

  .big {
    font-size: 2rem;
    margin-top: 8px;
    margin-bottom: 0;
  }

  .badges,
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .between {
    justify-content: space-between;
  }

  .badge,
  .pill {
    padding: 4px 8px;
    border-radius: 999px;
    border: 1px solid rgba(148, 163, 184, 0.35);
    font-size: 0.8rem;
  }

  .ok {
    color: #34d399;
    border-color: rgba(52, 211, 153, 0.4);
  }

  .warn {
    color: #f59e0b;
    border-color: rgba(245, 158, 11, 0.4);
  }

  .muted {
    color: #94a3b8;
  }

  .error {
    color: #fca5a5;
  }

  .top-gap {
    margin-top: 10px;
  }

  .hint {
    color: #94a3b8;
    margin-top: 4px;
    margin-bottom: 0;
    font-size: 0.9rem;
  }

  .list {
    margin-top: 10px;
    display: grid;
    gap: 10px;
  }

  .item {
    border: 1px solid rgba(148, 163, 184, 0.25);
    border-radius: 10px;
    padding: 10px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    flex-wrap: wrap;
  }

  input,
  button {
    border-radius: 8px;
    border: 1px solid rgba(148, 163, 184, 0.4);
    padding: 8px 10px;
    background: rgba(30, 41, 59, 0.8);
    color: #e2e8f0;
  }

  input {
    min-width: 220px;
    flex: 1;
  }

  button {
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .danger {
    background: rgba(127, 29, 29, 0.7);
    border-color: rgba(248, 113, 113, 0.5);
  }

  code {
    word-break: break-all;
  }

  .mono {
    font-family: Consolas, monospace;
    margin: 0;
  }

  .waiting-msg {
    color: #fcd34d;
    font-weight: 600;
  }

  .feature-actions button.selected {
    background: rgba(37, 99, 235, 0.8);
    border: 1px solid rgba(147, 197, 253, 0.9);
    color: #dbeafe;
  }

  .screen-preview {
    width: 100%;
    max-height: 480px;
    object-fit: contain;
    border-radius: 10px;
    border: 1px solid rgba(148, 163, 184, 0.3);
    background: rgba(2, 6, 23, 0.8);
  }

  .video-shell {
    position: relative;
    border-radius: 12px;
    overflow: hidden;
    background: #020617;
    border: 1px solid rgba(148, 163, 184, 0.25);
    min-height: 320px;
  }

  .viewer-video {
    width: 100%;
    height: auto;
    min-height: 320px;
    display: block;
    background: #020617;
    object-fit: contain;
  }

  .video-placeholder {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    padding: 16px;
    color: #cbd5e1;
    background: linear-gradient(180deg, rgba(2, 6, 23, 0.6), rgba(2, 6, 23, 0.9));
    pointer-events: none;
  }

  /* Approval Modal Styles */
  .approval-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .approval-modal {
    background: rgba(15, 23, 42, 0.95);
    border: 2px solid rgba(148, 163, 184, 0.3);
    border-radius: 16px;
    padding: 32px;
    max-width: 450px;
    width: 90%;
    backdrop-filter: blur(12px);
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.5);
  }

  .approval-modal h2 {
    margin: 0 0 16px 0;
    font-size: 1.5rem;
    color: #f1f5f9;
  }

  .approval-desc {
    margin: 0 0 24px 0;
    color: #cbd5e1;
    line-height: 1.6;
  }

  .approval-options {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 24px 0;
  }

  .approval-options label {
    display: flex;
    align-items: center;
    gap: 10px;
    cursor: pointer;
    padding: 8px;
    border-radius: 8px;
    transition: background 0.2s;
  }

  .approval-options label:hover {
    background: rgba(148, 163, 184, 0.1);
  }

  .approval-options input[type="checkbox"] {
    width: 18px;
    height: 18px;
    cursor: pointer;
    accent-color: #3b82f6;
  }

  .approval-options input[type="checkbox"]:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .approval-actions {
    display: flex;
    gap: 12px;
    margin-top: 24px;
  }

  .btn-reject,
  .btn-approve {
    flex: 1;
    padding: 12px 16px;
    border: none;
    border-radius: 8px;
    font-size: 1rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.2s;
  }

  .btn-reject {
    background: rgba(127, 29, 29, 0.7);
    color: #fecaca;
    border: 1px solid rgba(248, 113, 113, 0.5);
  }

  .btn-reject:hover:not(:disabled) {
    background: rgba(153, 27, 27, 0.8);
    border-color: rgba(252, 165, 165, 0.7);
  }

  .btn-approve {
    background: rgba(30, 64, 175, 0.7);
    color: #93c5fd;
    border: 1px solid rgba(59, 130, 246, 0.5);
  }

  .btn-approve:hover:not(:disabled) {
    background: rgba(37, 99, 235, 0.8);
    border-color: rgba(147, 197, 253, 0.7);
  }

  .btn-reject:disabled,
  .btn-approve:disabled {
    opacity: 0.7;
    cursor: not-allowed;
  }

  @media (max-width: 700px) {
    main {
      padding: 14px;
    }

    input {
      min-width: 0;
      width: 100%;
    }

    .approval-modal {
      padding: 24px;
    }
  }
</style>
