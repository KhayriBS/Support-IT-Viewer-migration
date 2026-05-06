<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import { ChatRealtimeClient, SignalingClient, technicianApi } from "$lib/api";
  import type { Agent, ChatMessage, ControlSession, SignalMessage, TypingNotification } from "$lib/api";
  import type { FileEntry, FileTransfer } from "$lib/api/types";

  interface AgentMetrics {
    cpuUsage: number;
    ramUsage: number;
    diskUsage: number;
    timestamp: number;
  }

  let metrics = $state<AgentMetrics | null>(null);
  let metricsError = $state<string | null>(null);
  let metricsLoading = $state(true);
  let metricsPanelOpen = $state(false);
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

  // ── Historique des sessions : alimenté par l'API backend ──
  // GET /sessions/history/{machineId}?direction=&status=&q=
  // (cf. SessionController côté Spring)
  let rdSessionHistory = $state<import("$lib/api/types").SessionHistoryEntry[]>([]);
  let rdSessionLoading = $state(false);
  let rdSessionError = $state<string | null>(null);
  let rdSessionSearch = $state("");
  let rdSessionTypeFilter = $state<"all" | "incoming" | "outgoing">("all");
  let rdSessionStatusFilter = $state<"all" | "active" | "ended">("all");
  let rdSessionRefreshTimer: ReturnType<typeof setTimeout> | null = null;

  // Fichiers : recherche + filtre transfert
  let rdFileSearch = $state("");
  let rdFileFilter = $state<"all" | "upload" | "download">("all");

  const signalingClient = new SignalingClient();
  const chatClient = new ChatRealtimeClient();

  // â”€â”€ Diagnostic logger (always on â€” strip these once issue is fixed) â”€â”€â”€â”€â”€â”€â”€â”€
  // Goal: see in DevTools console exactly which event/branch fires when the
  // session dies. Prefix every log with [DIAG] for easy grep.
  // Note: we deep-clone payload via JSON to break Svelte 5 $state proxies
  // (which would otherwise trigger the `console_log_state` warning).
  function diag(tag: string, payload?: unknown) {
    if (payload === undefined) {
      console.log(`[DIAG] ${tag}`);
      return;
    }
    let safe: unknown = payload;
    try {
      safe = JSON.parse(JSON.stringify(payload));
    } catch {
      // payload contains non-serializable values (Map, RTCPeerConnection, â€¦) â€”
      // log as-is, the proxy warning is harmless.
    }
    console.log(`[DIAG] ${tag}`, safe);
  }

  // Inbound video stats poller â€” confirms whether bytes/frames actually arrive.
  // If bytesReceived stays at 0 â†’ media path is broken (codec/SRTP/FEC/etc.).
  // If bytesReceived grows but framesDecoded == 0 â†’ decoder rejecting frames.
  let inboundStatsTimer: ReturnType<typeof setInterval> | null = null;
  function startInboundStatsLogger(pc: RTCPeerConnection) {
    stopInboundStatsLogger();
    let lastBytes = 0;
    let lastFrames = 0;
    inboundStatsTimer = setInterval(async () => {
      try {
        const stats = await pc.getStats();
        stats.forEach((s) => {
          if (s.type === "inbound-rtp" && (s as RTCInboundRtpStreamStats).kind === "video") {
            const r = s as RTCInboundRtpStreamStats & {
              bytesReceived?: number;
              framesReceived?: number;
              framesDecoded?: number;
              framesDropped?: number;
              packetsLost?: number;
              jitter?: number;
            };
            const bytes = r.bytesReceived ?? 0;
            const frames = r.framesReceived ?? 0;
            diag("inbound-rtp video", {
              bytesReceived: bytes,
              deltaBytes: bytes - lastBytes,
              framesReceived: frames,
              deltaFrames: frames - lastFrames,
              framesDecoded: r.framesDecoded ?? 0,
              framesDropped: r.framesDropped ?? 0,
              packetsLost: r.packetsLost ?? 0,
              jitter: r.jitter ?? 0
            });
            lastBytes = bytes;
            lastFrames = frames;
          }
        });
      } catch (err) {
        diag("getStats failed", String(err));
      }
    }, 2000);
  }
  function stopInboundStatsLogger() {
    if (inboundStatsTimer) {
      clearInterval(inboundStatsTimer);
      inboundStatsTimer = null;
    }
  }

  // Watchdog: after the signaling socket dies mid-ICE, give the peer N seconds
  // to reach `connected` on its own (most ICE candidates were already exchanged
  // before the close). If not, declare failure and clean up.
  let iceWatchdogTimer: ReturnType<typeof setTimeout> | null = null;
  const ICE_CONVERGENCE_WINDOW_MS = 15000; // bumped â€” ICE relay can take time
  async function dumpIceCandidatePairs(pc: RTCPeerConnection) {
    try {
      const stats = await pc.getStats();
      const candidates = new Map<string, RTCStats>();
      const pairs: Array<{
        state: string;
        nominated: boolean;
        local?: { type?: string; protocol?: string; address?: string };
        remote?: { type?: string; protocol?: string; address?: string };
        bytesSent?: number;
        bytesReceived?: number;
      }> = [];
      stats.forEach((s) => {
        if (s.type === "local-candidate" || s.type === "remote-candidate") {
          candidates.set(s.id, s as RTCStats);
        }
      });
      stats.forEach((s) => {
        if (s.type === "candidate-pair") {
          const p = s as RTCIceCandidatePairStats & { localCandidateId?: string; remoteCandidateId?: string };
          const local = p.localCandidateId ? candidates.get(p.localCandidateId) : undefined;
          const remote = p.remoteCandidateId ? candidates.get(p.remoteCandidateId) : undefined;
          pairs.push({
            state: p.state ?? "?",
            nominated: !!p.nominated,
            local: local && {
              type: (local as { candidateType?: string }).candidateType,
              protocol: (local as { protocol?: string }).protocol,
              address: (local as { address?: string }).address
            },
            remote: remote && {
              type: (remote as { candidateType?: string }).candidateType,
              protocol: (remote as { protocol?: string }).protocol,
              address: (remote as { address?: string }).address
            },
            bytesSent: (p as { bytesSent?: number }).bytesSent,
            bytesReceived: (p as { bytesReceived?: number }).bytesReceived
          });
        }
      });
      diag("ICE candidate pairs", pairs);
    } catch (err) {
      diag("dumpIceCandidatePairs failed", String(err));
    }
  }

  function startIceConvergenceWatchdog() {
    stopIceConvergenceWatchdog();
    iceWatchdogTimer = setTimeout(() => {
      iceWatchdogTimer = null;
      const pc = viewerPeerConnection;
      const state = pc?.connectionState;
      if (state === "connected") {
        diag("ICE watchdog: peer is connected, all good");
        signalingError = null;
        return;
      }
      diag("ICE watchdog EXPIRED â€” peer never reached connected", { state });
      // Last-resort dump: shows which candidate pairs ICE actually tried.
      // Look for state="succeeded" or "failed", and presence of "relay" type.
      if (pc) void dumpIceCandidatePairs(pc);
      signalingError =
        "Connexion vidÃ©o impossible aprÃ¨s perte du signaling. Recharge la session.";
      resetViewerPeerConnection();
      if (backendSessionSynced) {
        void leaveBackendSession();
      }
    }, ICE_CONVERGENCE_WINDOW_MS);
  }
  function stopIceConvergenceWatchdog() {
    if (iceWatchdogTimer) {
      clearTimeout(iceWatchdogTimer);
      iceWatchdogTimer = null;
    }
  }

  // ICE recovery: when the media path drops after initial success, prefer
  // ICE restart over hard peer reset.
  let iceRestartTimer: ReturnType<typeof setTimeout> | null = null;
  let iceRestartInFlight = false;
  const ICE_RESTART_ON_DISCONNECTED_DELAY_MS = 5000;
  const ICE_RESTART_ON_FAILED_DELAY_MS = 1200;

  function stopIceRestartTimer() {
    if (iceRestartTimer) {
      clearTimeout(iceRestartTimer);
      iceRestartTimer = null;
    }
  }

  function scheduleIceRestart(reason: string, delayMs: number) {
    if (signalingManualDisconnect || signalingRemoteEnded) {
      return;
    }
    if (!viewerPeerConnection || viewerPeerConnection.connectionState === "closed") {
      return;
    }
    if (iceRestartInFlight || iceRestartTimer) {
      return;
    }

    diag("ICE restart scheduled", {
      reason,
      delayMs,
      signalingConnected,
      peerState: viewerPeerConnection.connectionState,
      iceState: viewerPeerConnection.iceConnectionState
    });

    iceRestartTimer = setTimeout(() => {
      iceRestartTimer = null;
      void attemptIceRestart(reason);
    }, delayMs);
  }

  async function attemptIceRestart(reason: string) {
    if (iceRestartInFlight) {
      return;
    }

    const session = queriedSession ?? activeSession;
    if (!session || session.status !== "ACTIVE") {
      diag("ICE restart aborted â€” no active session", { reason });
      return;
    }

    const pc = viewerPeerConnection;
    if (!pc || pc.connectionState === "closed") {
      diag("ICE restart aborted â€” peer unavailable", { reason });
      return;
    }

    iceRestartInFlight = true;
    try {
      if (!signalingConnected) {
        diag("ICE restart: signaling down, reconnect requested", { reason });
        await connectSignaling({ force: true, reason: "ice_restart" });
      }
      if (!signalingConnected) {
        diag("ICE restart aborted â€” signaling still unavailable");
        scheduleIceRestart("signaling_unavailable", 2500);
        return;
      }

      const livePc = viewerPeerConnection;
      if (!livePc || livePc !== pc || livePc.signalingState === "closed") {
        diag("ICE restart aborted â€” peer changed/closed", { reason });
        return;
      }

      if (livePc.signalingState !== "stable") {
        diag("ICE restart deferred â€” signalingState not stable", {
          signalingState: livePc.signalingState
        });
        scheduleIceRestart("deferred_non_stable", 1200);
        return;
      }

      diag("ICE restart starting", {
        reason,
        peerState: livePc.connectionState,
        iceState: livePc.iceConnectionState
      });

      livePc.restartIce();
      const offer = await livePc.createOffer({
        iceRestart: true,
        offerToReceiveVideo: true
      });
      await livePc.setLocalDescription(offer);
      await waitForIceGathering(livePc, 3000);

      const finalSdp = livePc.localDescription?.sdp ?? offer.sdp ?? "";
      const finalType = livePc.localDescription?.type ?? offer.type;
      const restartOffer = { type: finalType, sdp: finalSdp };

      sendViewerOfferPayload(String(session.id), restartOffer);
      startViewerOfferRetry(String(session.id), restartOffer);
      screenFrameError = "Reconnexion reseau en cours (ICE restart)...";
      diag("ICE restart offer sent", {
        sessionId: session.id,
        candidateCount: finalSdp.split("\n").filter((l) => l.startsWith("a=candidate")).length
      });
    } catch (error) {
      diag("ICE restart failed", String(error));
      scheduleIceRestart("retry_after_error", 2500);
    } finally {
      iceRestartInFlight = false;
    }
  }

  const uiDebugEnabled =
    import.meta.env.DEV &&
    String((import.meta as unknown as { env?: Record<string, unknown> }).env?.VITE_UI_DEBUG ?? "") === "1";
  let signalingConnected = $state(false);
  let signalingError = $state<string | null>(null);
  let backendSessionSynced = $state(false);
  let backendSyncError = $state<string | null>(null);
  let signalLogs = $state<SignalLogEntry[]>([]);
  let detachMessageListener: (() => void) | null = null;
  let detachCloseListener: (() => void) | null = null;
  let detachErrorListener: (() => void) | null = null;
  let signalingManualDisconnect = false;
  let signalingRemoteEnded = false;
  let signalingReconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let signalingReconnectAttempts = 0;
  let viewerHadConnectedOnce = false;
  // True while a connectSignaling() call is still opening its WebSocket.
  // Prevents two parallel connects on the same token (HMR re-mount, double
  // chooseFeature, etc.) which the server rejects with 1003.
  let connectSignalingInFlight = false;
  // Buffer for local ICE candidates generated AFTER the signaling socket dies.
  // The flaky server closes the WS before all candidates (especially `relay`
  // ones via TURN, which take time to allocate) are emitted by the browser.
  // We keep them around so we can flush them if signaling ever comes back.
  let bufferedLocalIceCandidates: SignalMessage[] = [];
  let viewerPeerConnection: RTCPeerConnection | null = null;
  let viewerControlChannel: RTCDataChannel | null = null;
  let viewerSignalProcessing: Promise<void> = Promise.resolve();
  let pendingViewerIceCandidates: RTCIceCandidateInit[] = [];
  let viewerAnswerReceived = false;
  let viewerOfferRetryTimer: ReturnType<typeof setInterval> | null = null;
  let viewerOfferRetryCount = 0;
  // Le retry continue tant qu'une session est ACTIVE — pas de cap arbitraire.
  // L'ancien cap à 8 (= 8s) faisait abandonner trop vite quand Render fermait
  // la WS signaling pendant l'OFFER et que l'agent re-écoutait après reconnect.
  const maxViewerOfferRetries = 60;
  let viewerControlsTimer: ReturnType<typeof setTimeout> | null = null;
  let detachViewerInputListeners: (() => void) | null = null;
  let detachViewerFullscreenListener: (() => void) | null = null;
  let viewerShellEl = $state<HTMLDivElement | null>(null);
  let viewerVideoEl = $state<HTMLVideoElement | null>(null);
  let viewerRemoteStream = $state<MediaStream | null>(null);
  let viewerDataChannelOpen = $state(false);
  let viewerKeyboardCaptured = $state(false);
  let lastViewerMouseMoveSentAt = 0;
  let lastViewerWheelSentAt = 0;
  let lastViewerPointerSent: { x: number; y: number } | null = null;
  const viewerMouseMoveMinIntervalMs = 1000 / 90;
  const viewerWheelMinIntervalMs = 1000 / 60;
  let viewerConnectionState = $state<string>("idle");
  let viewerControlsVisible = $state(true);
  let viewerExpanded = $state(false);
  let viewerFullscreenActive = $state(false);
  let viewerRemoteWidth = $state(1920);
  let viewerRemoteHeight = $state(1080);
  // Metered "global.relay" static credentials (account: lumieretech).
  // Hard fallback used when VITE_ICE_SERVERS is unset and the Tauri
  // get_ice_servers_cmd returns nothing usable. Rotate if Metered invalidates.
  const defaultViewerIceServers: RTCIceServer[] = [
    { urls: "stun:stun.relay.metered.ca:80" },
    {
      urls: "turn:global.relay.metered.ca:80",
      username: "d156f70e60e74c734ec39dc8",
      credential: "Z9zO5Kp3c5P/c6e0"
    },
    {
      urls: "turn:global.relay.metered.ca:80?transport=tcp",
      username: "d156f70e60e74c734ec39dc8",
      credential: "Z9zO5Kp3c5P/c6e0"
    },
    {
      urls: "turn:global.relay.metered.ca:443",
      username: "d156f70e60e74c734ec39dc8",
      credential: "Z9zO5Kp3c5P/c6e0"
    },
    {
      urls: "turns:global.relay.metered.ca:443?transport=tcp",
      username: "d156f70e60e74c734ec39dc8",
      credential: "Z9zO5Kp3c5P/c6e0"
    }
  ];
  let viewerIceServers = $state<RTCIceServer[]>(defaultViewerIceServers);
  let viewerStreamMbps = $state<number | null>(null);
  let viewerStreamFps = $state<number | null>(null);
  let viewerPlaybackProfile = $state<"responsive" | "quality">("responsive");
  let viewerFpsTier = $state<"auto" | "idle" | "normal" | "active">("auto");
  let viewerBitrateTier = $state<"auto" | "poor" | "medium" | "good">("auto");
  let viewerPreset = $state<"custom" | "low-latency" | "balanced" | "quality">("balanced");
  let viewerProfileManualOverride = false;
  let viewerProfileAutoUpgradeTimer: ReturnType<typeof setTimeout> | null = null;
  const viewerAutoUpgradeDelayMs = 7000;
  const viewerAutoUpgradeMinMbps = 1.8;
  const viewerAutoUpgradeMinFps = 28;
  const streamProfileSignalEnabled =
    String((import.meta.env.VITE_ENABLE_STREAM_PROFILE_SIGNAL ?? "true")).trim().toLowerCase() !== "false";
  let screenFrameError = $state<string | null>(null);

  interface RemoteInputEvent {
    type: string;
    x?: number;
    y?: number;
    button?: number;
    deltaY?: number;
    key?: string;
    code?: string;
    keyCode?: number;
    modifiers?: {
      ctrl?: boolean;
      alt?: boolean;
      shift?: boolean;
      meta?: boolean;
    };
  }

  // â”€â”€ File transfer via WebRTC DataChannel "file" â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
  let fileChannel: RTCDataChannel | null = null;
  let fileChannelOpen = $state(false);
  let fileListLoading = $state(false);
  let fileListError = $state<string | null>(null);
  let fileCurrentPath = $state("");
  let fileListing = $state<FileEntry[]>([]);
  let fileTransfers = $state<Record<string, FileTransfer>>({});
  /** transferId of the download currently receiving binary chunks */
  let activeDownloadId: string | null = null;
  const FILE_CHANNEL_UPLOAD_BACKPRESSURE = 4 * 1024 * 1024; // 4 MB
  // 16 KB par chunk : largement sous toutes les limites SCTP/WebRTC
  // (certaines impls plantent à 64 KB après plusieurs transferts).
  const FILE_CHUNK_SIZE = 16 * 1024;                         // 16 KB

  let metricsTimer: ReturnType<typeof setInterval>;
  let agentsTimer: ReturnType<typeof setInterval>;
  let chatPollTimer: ReturnType<typeof setInterval> | null = null;

  let chatConnected = $state(false);
  let chatRoomId = $state("");
  let chatInput = $state("");
  let chatMessages = $state<ChatMessage[]>([]);
  let chatError = $state<string | null>(null);
  let typingInfo = $state<TypingNotification | null>(null);
  let typingClearTimer: ReturnType<typeof setTimeout> | null = null;
  let detachChatMessageListener: (() => void) | null = null;
  let detachChatTypingListener: (() => void) | null = null;
  let detachChatConnectionListener: (() => void) | null = null;

  // Session approval modal
  let machineId = $state<string>("");
  let localMachineId = $state<string>("");
  let localConnectionCode = $state<string>("");
  let localConnectionCodeLoading = $state(false);
  let localConnectionCodeError = $state<string | null>(null);
  let connectionCodeCopied = $state(false);
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

  // ── Historique sessions : appel direct à l'API Spring ──
  // L'API filtre côté serveur (direction / status / q). On debounce le fetch
  // pour ne pas spammer le backend pendant que l'utilisateur tape.
  async function fetchSessionHistory() {
    // Priorité au connection_code (ce que la table backend connaît directement),
    // fallback sur le machineId si le code n'est pas encore chargé.
    const key = (localConnectionCode || localMachineId || "").trim();
    if (!key) {
      rdSessionHistory = [];
      return;
    }
    rdSessionLoading = true;
    rdSessionError = null;
    try {
      const list = await technicianApi.getSessionHistory(key, {
        direction: rdSessionTypeFilter,
        status: rdSessionStatusFilter,
        q: rdSessionSearch
      });
      rdSessionHistory = list;
    } catch (err) {
      rdSessionError = String(err);
      rdSessionHistory = [];
    } finally {
      rdSessionLoading = false;
    }
  }

  $effect(() => {
    // dépendances réactives
    void localConnectionCode;
    void localMachineId;
    void rdSessionTypeFilter;
    void rdSessionStatusFilter;
    void rdSessionSearch;
    if (rdSessionRefreshTimer) clearTimeout(rdSessionRefreshTimer);
    rdSessionRefreshTimer = setTimeout(() => { void fetchSessionHistory(); }, 250);
  });

  // Refresh immédiat quand une session vient de démarrer / se terminer.
  $effect(() => {
    void activeSession?.id;
    void activeSession?.status;
    void pendingApprovalSession?.id;
    void fetchSessionHistory();
  });

  // ── Listes filtrées dérivées ──
  // Les sessions sont déjà filtrées côté backend, on les affiche telles quelles.
  // (La recherche/filtre côté client est conservée comme fallback en attendant
  // que le fetch debounced revienne.)
  const rdFilteredSessions = $derived(rdSessionHistory);

  const rdFilteredFiles = $derived.by(() => {
    const search = rdFileSearch.trim().toLowerCase();
    const list = Object.values(fileTransfers);
    return list
      .filter((f) => {
        if (rdFileFilter !== "all" && f.type !== rdFileFilter) return false;
        if (search && !f.fileName.toLowerCase().includes(search)) return false;
        return true;
      })
      .sort((a, b) => b.startedAt - a.startedAt);
  });

  function rdFormatDuration(ms: number | null): string {
    if (!ms || ms <= 0) return "-";
    const total = Math.floor(ms / 1000);
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    if (h > 0) return `${h}h ${m}min`;
    return `${m} min`;
  }
  function rdFormatTime(iso: string): string {
    try {
      return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    } catch {
      return "-";
    }
  }
  function rdFormatRelative(ms: number): string {
    const diff = Date.now() - ms;
    const m = Math.floor(diff / 60000);
    if (m < 1) return "à l'instant";
    if (m < 60) return `Il y a ${m} min`;
    const h = Math.floor(m / 60);
    if (h < 24) return `Il y a ${h}h`;
    const d = Math.floor(h / 24);
    return `Il y a ${d}j`;
  }
  function rdFormatBytes(b: number): string {
    if (!b || b < 1024) return `${b} B`;
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
    if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} MB`;
    return `${(b / 1024 / 1024 / 1024).toFixed(1)} GB`;
  }
  function rdFileIconClass(name: string): string {
    const lower = name.toLowerCase();
    if (lower.endsWith(".pdf")) return "rd-file__icon--pdf";
    if (lower.endsWith(".pptx") || lower.endsWith(".ppt")) return "rd-file__icon--ppt";
    if (lower.endsWith(".zip") || lower.endsWith(".rar") || lower.endsWith(".7z")) return "rd-file__icon--zip";
    return "rd-file__icon--pdf";
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
      await refreshLocalConnectionCode();

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

  async function refreshLocalConnectionCode() {
    const machineId = localMachineId.trim();
    if (!machineId) {
      localConnectionCode = "";
      localConnectionCodeError = null;
      return;
    }

    localConnectionCodeLoading = true;
    try {
      const response = await technicianApi.getMachineAuthStatus(machineId);
      localConnectionCode = response?.data?.connectionCode?.trim?.() ?? "";
      localConnectionCodeError = null;
    } catch (error) {
      localConnectionCode = "";
      localConnectionCodeError = String(error);
    } finally {
      localConnectionCodeLoading = false;
    }
  }

  async function copyLocalConnectionCode() {
    if (!localConnectionCode) {
      return;
    }

    try {
      await navigator.clipboard.writeText(localConnectionCode);
      connectionCodeCopied = true;
      setTimeout(() => {
        connectionCodeCopied = false;
      }, 1600);
    } catch {
      connectionCodeCopied = false;
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
          // PAS d'auto-screen : laisse l'utilisateur choisir Écran/Fichier/Chat
          // depuis le menu post-connexion. selectedFeature reste null.
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
      await disconnectSignaling({ sendLeave: true });
      disconnectChat(); // tear down STOMP/poll when session ends
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

  // â”€â”€ Dedup helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

  /** Stable identity key. Prefers server id; falls back to content fingerprint. */
  function msgKey(msg: ChatMessage): string {
    if (msg.id !== undefined && msg.id !== null) {
      return `id:${msg.id}`;
    }
    return `${msg.senderName}:${msg.timestamp}:${msg.content.slice(0, 64)}`;
  }

  /** Merge two message arrays without duplicates, keeping chronological order. */
  function mergeMessages(existing: ChatMessage[], incoming: ChatMessage[]): ChatMessage[] {
    if (incoming.length === 0) return existing;
    const seen = new Set(existing.map(msgKey));
    const merged = [...existing];
    for (const msg of incoming) {
      const k = msgKey(msg);
      if (!seen.has(k)) {
        merged.push(msg);
        seen.add(k);
      }
    }
    // Sort by timestamp (ISO strings sort lexicographically)
    merged.sort((a, b) => a.timestamp.localeCompare(b.timestamp));
    return merged.slice(-200);
  }

  // â”€â”€ Poll timer helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

  function startChatPoll() {
    if (chatPollTimer) return; // already running
    chatPollTimer = setInterval(() => {
      void refreshChatMessages();
    }, 5000);
  }

  function stopChatPoll() {
    if (chatPollTimer) {
      clearInterval(chatPollTimer);
      chatPollTimer = null;
    }
  }

  // â”€â”€ Core chat functions â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

  async function refreshChatMessages(roomOverride?: string, replace = false) {
    const roomId = roomOverride || chatRoomId || resolveRoomId();
    if (!roomId) return;

    try {
      const fetched = await technicianApi.getMessages(roomId);

      // Ignore stale responses when the user has switched to another session/room.
      if (chatRoomId && chatRoomId !== roomId) {
        return;
      }

      chatMessages = replace
        ? mergeMessages([], fetched)
        : mergeMessages(chatMessages, fetched);
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

    // Guard: same room and already connected â€” nothing to do
    if (chatRoomId === roomId && chatConnected) {
      return;
    }

    disconnectChat();
    chatMessages = [];
    typingInfo = null;
    chatRoomId = roomId;
    chatError = null;

    // STOMP message handler with inline dedup
    detachChatMessageListener = chatClient.onMessage((msg) => {
      const k = msgKey(msg);
      if (chatMessages.some((m) => msgKey(m) === k)) return;
      chatMessages = [...chatMessages, msg].slice(-200);
    });

    // Typing indicator with auto-clear
    detachChatTypingListener = chatClient.onTyping((msg) => {
      typingInfo = msg.isTyping ? msg : null;
      if (typingClearTimer) clearTimeout(typingClearTimer);
      if (msg.isTyping) {
        typingClearTimer = setTimeout(() => { typingInfo = null; }, 3000);
      }
    });

    // Connection state handler: drive poll timer based on STOMP availability
    detachChatConnectionListener = chatClient.onConnection((connected) => {
      chatConnected = connected;
      if (connected) {
        stopChatPoll(); // STOMP up â†’ no REST polling needed
      } else if (chatRoomId) {
        startChatPoll(); // STOMP dropped â†’ REST fallback kicks in
      }
    });

    // Initial message load (room-scoped replace to avoid cross-session bleed)
    await refreshChatMessages(roomId, true);

    try {
      await chatClient.connect(roomId);
    } catch (error) {
      chatError = String(error);
    }

    // Start poll as baseline fallback; stopped automatically when STOMP connects
    startChatPoll();
  }

  function disconnectChat() {
    // Clear room FIRST so the onConnection(false) handler does not restart the poll
    chatRoomId = "";
    chatClient.disconnect();
    chatConnected = false;
    stopChatPoll();
    if (typingClearTimer) { clearTimeout(typingClearTimer); typingClearTimer = null; }
    typingInfo = null;
    clearChatListeners();
  }

  async function sendChatMessage() {
    const roomId = chatRoomId || resolveRoomId();
    const content = chatInput.trim();
    if (!roomId || !content) return;

    // Guard: no active session
    const session = queriedSession ?? activeSession;
    if (!session || session.status !== "ACTIVE") {
      chatError = "Aucune session active.";
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
      // REST fallback: send then merge-refresh (server assigns timestamp/id)
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

  // â”€â”€ File DataChannel â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

  function configureFileDataChannel(channel: RTCDataChannel) {
    // Ensure binary data arrives as ArrayBuffer (not Blob)
    channel.binaryType = "arraybuffer";
    fileChannel = channel;
    fileChannelOpen = channel.readyState === "open";
    console.log(`[file-ch] configure (readyState=${channel.readyState})`);

    channel.onopen = () => {
      fileChannelOpen = true;
      console.log(`[file-ch] OPENED (readyState=${channel.readyState})`);
    };
    channel.onclose = () => {
      console.warn(`[file-ch] CLOSED (was fileChannel? ${fileChannel === channel}, readyState=${channel.readyState})`);
      if (fileChannel === channel) {
        fileChannelOpen = false;
        activeDownloadId = null;
      }
    };
    channel.onerror = (e) => {
      console.warn(`[file-ch] ERROR`, e);
      if (fileChannel === channel) {
        fileChannelOpen = false;
      }
    };
    channel.onmessage = (event: MessageEvent<string | ArrayBuffer>) => {
      if (typeof event.data === "string") {
        try {
          handleFileChannelJson(JSON.parse(event.data) as Record<string, unknown>);
        } catch {
          // ignore malformed JSON
        }
      } else if (event.data instanceof ArrayBuffer) {
        handleFileChannelBinary(event.data);
      }
    };
  }

  function handleFileChannelJson(msg: Record<string, unknown>) {
    const type = msg.type as string | undefined;
    const tid = (msg.transferId as string | undefined) ?? "";

    if (type === "FILE_LIST_RESPONSE") {
      fileCurrentPath = (msg.path as string) ?? "";
      fileListing = (msg.files as FileEntry[]) ?? [];
      fileListError = (msg.error as string | null) ?? null;
      fileListLoading = false;
      return;
    }

    if (type === "FILE_DOWNLOAD_RESPONSE") {
      activeDownloadId = tid;
      fileTransfers = {
        ...fileTransfers,
        [tid]: {
          transferId: tid,
          type: "download",
          fileName: (msg.fileName as string) ?? "file",
          totalSize: (msg.totalSize as number) ?? 0,
          totalChunks: (msg.totalChunks as number) ?? 1,
          doneChunks: 0,
          doneBytes: 0,
          startedAt: Date.now(),
          state: "active",
          buffers: []
        } satisfies FileTransfer
      };
      return;
    }

    if (type === "FILE_COMPLETE") {
      const transfer = fileTransfers[tid];
      if (transfer?.type === "download" && transfer.state === "active") {
        // Trigger browser download
        const blob = new Blob(transfer.buffers ?? []);
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = transfer.fileName;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        setTimeout(() => URL.revokeObjectURL(url), 60_000);

        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...transfer, state: "complete", buffers: undefined }
        };
        if (activeDownloadId === tid) {
          activeDownloadId = null;
        }
      } else if (transfer?.type === "upload") {
        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...transfer, state: "complete" }
        };
      }
      return;
    }

    if (type === "FILE_UPLOAD_STARTED") {
      // Debug : l'agent confirme avoir bien préparé l'upload + chemin de destination
      const destPath = (msg.destPath as string) ?? "";
      console.log(`[file-ch] agent confirms upload start tid=${tid} → ${destPath}`);
      const transfer = fileTransfers[tid];
      if (transfer) {
        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...transfer, error: undefined, ...(destPath ? { destPath } as Partial<FileTransfer> : {}) }
        };
      }
      return;
    }

    if (type === "FILE_UPLOAD_ACK") {
      const destPath = (msg.destPath as string) ?? "";
      const canonicalPath = (msg.canonicalPath as string) ?? "";
      const size = (msg.size as number) ?? 0;
      // Préfère le canonicalPath (résolu par OS, traverse junctions/symlinks)
      // sinon le destPath brut. Sur Windows FR ça donne le chemin RÉEL utilisable.
      const finalPath = canonicalPath || destPath;
      console.log(`[file-ch] agent ACK tid=${tid} size=${size} → ${finalPath}`);
      const transfer = fileTransfers[tid];
      if (transfer) {
        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...transfer, state: "complete", ...(finalPath ? { destPath: finalPath } as Partial<FileTransfer> : {}) }
        };
      }
      return;
    }

    if (type === "FILE_ERROR") {
      const transfer = fileTransfers[tid];
      if (transfer) {
        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...transfer, state: "error", error: (msg.message as string) ?? "unknown error" }
        };
      } else {
        console.error("[file-ch] remote error:", msg.message);
      }
      if (activeDownloadId === tid) {
        activeDownloadId = null;
      }
    }
  }

  function handleFileChannelBinary(data: ArrayBuffer) {
    const tid = activeDownloadId;
    if (!tid) return;
    const transfer = fileTransfers[tid];
    if (!transfer || transfer.type !== "download" || transfer.state !== "active") return;

    const updated: FileTransfer = {
      ...transfer,
      doneChunks: transfer.doneChunks + 1,
      doneBytes: transfer.doneBytes + data.byteLength,
      buffers: [...(transfer.buffers ?? []), data]
    };
    fileTransfers = { ...fileTransfers, [tid]: updated };
  }

  function requestFileList(path: string) {
    if (!fileChannel || fileChannel.readyState !== "open") {
      fileListError = "Canal fichier non disponible.";
      return;
    }
    fileListLoading = true;
    fileListError = null;
    fileChannel.send(JSON.stringify({ type: "FILE_LIST_REQUEST", path }));
  }

  function downloadRemoteFile(filePath: string, fileName: string) {
    if (!fileChannel || fileChannel.readyState !== "open") return;
    const tid = crypto.randomUUID();
    fileChannel.send(JSON.stringify({ type: "FILE_DOWNLOAD_REQUEST", transferId: tid, path: filePath }));
    // Transfer state is created when FILE_DOWNLOAD_RESPONSE arrives
    console.info("[file-ch] download requested:", fileName, tid);
  }

  async function uploadLocalFile(file: File) {
    if (!fileChannel || fileChannel.readyState !== "open") return;

    const tid = crypto.randomUUID();
    const totalChunks = Math.max(1, Math.ceil(file.size / FILE_CHUNK_SIZE));

    const transfer: FileTransfer = {
      transferId: tid,
      type: "upload",
      fileName: file.name,
      totalSize: file.size,
      totalChunks,
      doneChunks: 0,
      doneBytes: 0,
      startedAt: Date.now(),
      state: "active"
    };
    fileTransfers = { ...fileTransfers, [tid]: transfer };

    fileChannel.send(JSON.stringify({
      type: "FILE_UPLOAD_START",
      transferId: tid,
      fileName: file.name,
      totalSize: file.size,
      totalChunks
    }));

    for (let i = 0; i < totalChunks; i++) {
      if (!fileChannel || fileChannel.readyState !== "open") {
        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...fileTransfers[tid], state: "error", error: "Canal fermÃ© pendant l'envoi" }
        };
        return;
      }
      // Backpressure
      while (fileChannel.bufferedAmount > FILE_CHANNEL_UPLOAD_BACKPRESSURE) {
        await new Promise<void>((resolve) => setTimeout(resolve, 50));
      }

      const start = i * FILE_CHUNK_SIZE;
      const chunk = await file.slice(start, start + FILE_CHUNK_SIZE).arrayBuffer();
      try {
        fileChannel.send(chunk);
      } catch (sendErr) {
        console.error(`[file-ch] send chunk #${i + 1} failed:`, sendErr);
        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...fileTransfers[tid], state: "error", error: `send: ${String(sendErr)}` }
        };
        return;
      }

      const prev = fileTransfers[tid];
      fileTransfers = {
        ...fileTransfers,
        [tid]: {
          ...prev,
          doneChunks: i + 1,
          doneBytes: Math.min(prev.doneBytes + chunk.byteLength, file.size)
        }
      };
    }

    fileChannel.send(JSON.stringify({ type: "FILE_COMPLETE", transferId: tid }));

    // Tous les chunks ont été poussés sur la pipe SCTP. L'ACK FILE_UPLOAD_ACK
    // que l'agent renvoie peut être perdu (race entre fermeture rapide du
    // canal après transfert / reset peer). Plutôt que de laisser l'utilisateur
    // bloqué sur "active 100 %", on attend l'ACK 1 s puis on force "complete"
    // avec un flag "ackPending" — l'arrivée tardive de l'ACK reste idempotente.
    setTimeout(() => {
      const cur = fileTransfers[tid];
      if (cur && cur.state === "active" && cur.doneChunks >= totalChunks) {
        fileTransfers = {
          ...fileTransfers,
          [tid]: { ...cur, state: "complete", doneBytes: file.size }
        };
        console.log(`[file-ch] upload tid=${tid} forced to complete (ACK timeout)`);
      }
    }, 1000);
  }

  function resetFileChannel() {
    try { fileChannel?.close(); } catch { /* ignore */ }
    fileChannel = null;
    fileChannelOpen = false;
    fileListLoading = false;
    fileListError = null;
    fileListing = [];
    fileTransfers = {};
    activeDownloadId = null;
  }

  function formatFileSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

  function transferProgress(t: FileTransfer): number {
    if (t.totalSize === 0) return 100;
    return Math.round((t.doneBytes / t.totalSize) * 100);
  }

  function transferSpeed(t: FileTransfer): string {
    const elapsed = (Date.now() - t.startedAt) / 1000;
    if (elapsed < 0.1) return "";
    const bps = t.doneBytes / elapsed;
    return `${formatFileSize(Math.round(bps))}/s`;
  }

  // â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

  function viewerStateClass(state: string) {
    switch (state) {
      case "connected":
        return "ok";
      case "failed":
      case "disconnected":
      case "closed":
        return "error";
      case "connecting":
      case "new":
        return "warn";
      default:
        return "muted";
    }
  }

  function viewerStateLabel(state: string) {
    switch (state) {
      case "connected":
        return "connecte";
      case "connecting":
        return "connexion";
      case "disconnected":
        return "deconnecte";
      case "failed":
        return "echec";
      case "closed":
        return "ferme";
      case "new":
        return "initialisation";
      default:
        return "attente";
    }
  }

  function viewerQualityClass(mbps: number | null) {
    if (mbps === null) {
      return "muted";
    }

    if (mbps < 0.2) {
      return "error";
    }

    if (mbps < 0.7) {
      return "warn";
    }

    return "ok";
  }

  function viewerQualityLabel(mbps: number | null) {
    if (mbps === null) {
      return "qualite en attente";
    }

    if (mbps < 0.2) {
      return "qualite faible";
    }

    if (mbps < 0.7) {
      return "qualite moyenne";
    }

    if (mbps < 1.6) {
      return "qualite bonne";
    }

    return "qualite excellente";
  }

  function resolveIceServers(): RTCIceServer[] {
    const env = (import.meta as unknown as { env?: Record<string, unknown> }).env ?? {};
    const raw = typeof env.VITE_ICE_SERVERS === "string" ? env.VITE_ICE_SERVERS.trim() : "";

    if (!raw) {
      return defaultViewerIceServers;
    }

    if (raw.startsWith("[")) {
      try {
        const parsed = JSON.parse(raw) as unknown;
        if (Array.isArray(parsed)) {
          return parsed as RTCIceServer[];
        }
      } catch {
        // ignore parsing errors
      }
      return defaultViewerIceServers;
    }

    const urls = raw
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);

    if (urls.length === 0) {
      return defaultViewerIceServers;
    }

    return [{ urls }];
  }

  async function refreshViewerIceServers() {
    try {
      const servers = await invoke<Array<{ urls: string[] | string; username?: string; credential?: string }>>(
        "get_ice_servers_cmd"
      );

      if (!Array.isArray(servers) || servers.length === 0) {
        viewerIceServers = resolveIceServers();
        return;
      }

      const normalized: RTCIceServer[] = [];
      for (const server of servers) {
        const urls = Array.isArray(server.urls)
          ? server.urls.filter((url): url is string => typeof url === "string" && !!url.trim())
          : typeof server.urls === "string" && server.urls.trim()
            ? [server.urls.trim()]
            : [];

        if (urls.length === 0) {
          continue;
        }

        const iceServer: RTCIceServer = {
          urls: urls.length === 1 ? urls[0] : urls
        };

        if (typeof server.username === "string") {
          iceServer.username = server.username;
        }
        if (typeof server.credential === "string") {
          iceServer.credential = server.credential;
        }

        normalized.push(iceServer);
      }
      viewerIceServers = normalized;

      if (viewerIceServers.length === 0) {
        viewerIceServers = resolveIceServers();
      }
    } catch {
      viewerIceServers = resolveIceServers();
    }
  }

  function formatSignalPayload(type: SignalMessage["type"], payload: unknown) {
    if (payload === undefined || payload === null) {
      return "";
    }

    if (type === "OFFER" || type === "ANSWER") {
      const record = payload as Record<string, unknown>;
      const sdp = typeof record?.sdp === "string" ? record.sdp : "";
      const label = typeof record?.type === "string" ? record.type : type.toLowerCase();
      return `SDP ${label} â€¢ ${sdp.length} chars`;
    }

    if (type === "ICE") {
      const record = payload as Record<string, unknown>;
      const candidate = typeof record?.candidate === "string" ? record.candidate : "";
      return candidate.length > 96 ? `${candidate.slice(0, 96)}...` : candidate || "ICE candidate";
    }

    if (type === "STREAM_STATS") {
      const record = payload as Record<string, unknown>;
      const mbps = Number(record?.mbps ?? 0);
      const fps = Number(record?.fps ?? 0);
      return `${mbps.toFixed(2)} Mbps â€¢ ${fps.toFixed(1)} FPS`;
    }

    if (type === "FILE_DATA") {
      const record = payload as Record<string, unknown>;
      if (typeof record?.chunkIndex === "number") {
        return `File chunk ${record.chunkIndex}`;
      }
      return "File data";
    }

    const payloadText = JSON.stringify(payload);
    return payloadText.length > 180 ? `${payloadText.slice(0, 180)}...` : payloadText;
  }

  function logSignal(direction: "in" | "out", msg: SignalMessage) {
    if (!uiDebugEnabled) {
      return;
    }

    if (msg.type === "FILE_DATA" || msg.type === "STREAM_STATS") {
      return;
    }

    const next: SignalLogEntry = {
      timestamp: new Date().toLocaleTimeString(),
      direction,
      type: msg.type,
      from: msg.from ?? "",
      to: msg.to,
      payload: formatSignalPayload(msg.type, msg.payload)
    };

    signalLogs = [next, ...signalLogs].slice(0, 16);
  }

  function clearSignalingListeners() {
    detachMessageListener?.();
    detachCloseListener?.();
    detachErrorListener?.();
    detachMessageListener = null;
    detachCloseListener = null;
    detachErrorListener = null;
  }

  function stopSignalingReconnect() {
    if (signalingReconnectTimer) {
      clearTimeout(signalingReconnectTimer);
      signalingReconnectTimer = null;
    }
  }

  function scheduleSignalingReconnect() {
    stopSignalingReconnect();
    const current = queriedSession ?? activeSession;
    if (!current || current.status !== "ACTIVE") {
      return;
    }

    signalingReconnectAttempts += 1;
    const delayMs = Math.min(1000 * 2 ** (signalingReconnectAttempts - 1), 10000);
    signalingReconnectTimer = setTimeout(() => {
      signalingReconnectTimer = null;
      void connectSignaling();
    }, delayMs);
  }

  function isRetryableSignalingCloseCode(code: number) {
    // Retry only for transient network/server conditions.
    // 1006: abnormal closure (network loss)
    // 1011/1012/1013: server/internal temporary conditions
    return code === 1006 || code === 1011 || code === 1012 || code === 1013;
  }

  function stopViewerOfferRetry() {
    if (viewerOfferRetryTimer) {
      clearInterval(viewerOfferRetryTimer);
      viewerOfferRetryTimer = null;
    }
  }

  function stopViewerControlsAutoHide() {
    if (viewerControlsTimer) {
      clearTimeout(viewerControlsTimer);
      viewerControlsTimer = null;
    }
  }

  function revealViewerControls() {
    viewerControlsVisible = true;

    if (viewerConnectionState !== "connected") {
      stopViewerControlsAutoHide();
      return;
    }

    stopViewerControlsAutoHide();
    viewerControlsTimer = setTimeout(() => {
      viewerControlsVisible = false;
    }, 3000);
  }

  function configureViewerDataChannel(channel: RTCDataChannel) {
    viewerControlChannel = channel;
    viewerDataChannelOpen = channel.readyState === "open";

    channel.onopen = () => {
      viewerControlChannel = channel;
      viewerDataChannelOpen = true;
      screenFrameError = null;
      // Force le ré-envoi du PAUSE/RESUME : si l'effet a déjà tenté d'envoyer
      // alors que le canal n'était pas open, sa valeur a été silencieusement
      // perdue. Reset le tracking pour que l'effet le refasse.
      rdLastSentPaused = null;
    };

    channel.onclose = () => {
      if (viewerControlChannel === channel) {
        viewerDataChannelOpen = false;
        viewerKeyboardCaptured = false;
      }
    };

    channel.onerror = () => {
      if (viewerControlChannel === channel) {
        viewerDataChannelOpen = false;
        viewerKeyboardCaptured = false;
      }
    };
  }

  function canSendViewerInput() {
    const current = queriedSession ?? activeSession;
    return (
      selectedFeature === "screen" &&
      current?.status === "ACTIVE" &&
      current.allowRemoteInput !== false &&
      viewerDataChannelOpen &&
      !!viewerControlChannel
    );
  }

  function canSendViewerKeyboardInput() {
    return canSendViewerInput() && viewerKeyboardCaptured;
  }

  function sendViewerInput(event: RemoteInputEvent) {
    if (!canSendViewerInput() || !viewerControlChannel) {
      return false;
    }

    try {
      viewerControlChannel.send(JSON.stringify(event));
      return true;
    } catch {
      viewerDataChannelOpen = false;
      viewerKeyboardCaptured = false;
      return false;
    }
  }

  function syncViewerVideoMetadata(videoEl: HTMLVideoElement) {
    if (videoEl.videoWidth > 0 && videoEl.videoHeight > 0) {
      viewerRemoteWidth = videoEl.videoWidth;
      viewerRemoteHeight = videoEl.videoHeight;
    }
  }

  function viewerPlayoutDelayHint(): number {
    return viewerPlaybackProfile === "quality" ? 0.12 : 0.0;
  }

  function applyViewerJitterBufferProfile(pc: RTCPeerConnection | null) {
    if (!pc) {
      return;
    }

    const playoutDelay = viewerPlayoutDelayHint();
    for (const receiver of pc.getReceivers()) {
      if (receiver.track?.kind !== "video") {
        continue;
      }

      const receiverWithHint = receiver as RTCRtpReceiver & { playoutDelayHint?: number };
      try {
        receiverWithHint.playoutDelayHint = playoutDelay;
      } catch {
        // Some browsers expose playoutDelayHint as readonly or behind flags.
      }
    }
  }

  function stopViewerAutoUpgradeTimer() {
    if (viewerProfileAutoUpgradeTimer) {
      clearTimeout(viewerProfileAutoUpgradeTimer);
      viewerProfileAutoUpgradeTimer = null;
    }
  }

  function sendViewerPlaybackProfile(
    profile: "responsive" | "quality",
    options?: { manualOverride?: boolean }
  ) {
    viewerPlaybackProfile = profile;
    if (options?.manualOverride) {
      viewerProfileManualOverride = true;
    }

    applyViewerJitterBufferProfile(viewerPeerConnection);

    if (!streamProfileSignalEnabled) {
      return;
    }

    const current = queriedSession ?? activeSession;
    if (!signalingConnected || !current?.id) {
      return;
    }

    const bitrateBpsByTier: Record<"poor" | "medium" | "good", number> = {
      poor: 1_500_000,
      medium: 4_000_000,
      good: 8_000_000
    };

    const bitrateBps = viewerBitrateTier === "auto" ? undefined : bitrateBpsByTier[viewerBitrateTier];
    const fpsTier = viewerFpsTier === "auto" ? undefined : viewerFpsTier;

    const profileMessage: SignalMessage = {
      type: "STREAM_PROFILE",
      to: "agent",
      sessionId: String(current.id),
      payload: {
        profile,
        bitrateBps,
        fpsTier
      }
    };

    try {
      signalingClient.send(profileMessage, "viewer");
      logSignal("out", { ...profileMessage, from: "viewer" });
    } catch {
      // Ignore transient signaling send issues.
    }
  }

  function maybeAutoUpgradeViewerProfile() {
    const isEligible =
      signalingConnected &&
      viewerConnectionState === "connected" &&
      viewerPlaybackProfile === "responsive" &&
      !viewerProfileManualOverride &&
      (viewerStreamMbps ?? 0) >= viewerAutoUpgradeMinMbps &&
      (viewerStreamFps ?? 0) >= viewerAutoUpgradeMinFps;

    if (!isEligible) {
      stopViewerAutoUpgradeTimer();
      return;
    }

    if (viewerProfileAutoUpgradeTimer) {
      return;
    }

    viewerProfileAutoUpgradeTimer = setTimeout(() => {
      viewerProfileAutoUpgradeTimer = null;

      const stillEligible =
        signalingConnected &&
        viewerConnectionState === "connected" &&
        viewerPlaybackProfile === "responsive" &&
        !viewerProfileManualOverride &&
        (viewerStreamMbps ?? 0) >= viewerAutoUpgradeMinMbps &&
        (viewerStreamFps ?? 0) >= viewerAutoUpgradeMinFps;

      if (!stillEligible) {
        return;
      }

      sendViewerPlaybackProfile("quality");
    }, viewerAutoUpgradeDelayMs);
  }

  function toggleViewerPlaybackProfile() {
    stopViewerAutoUpgradeTimer();
    const nextProfile = viewerPlaybackProfile === "quality" ? "responsive" : "quality";
    viewerPreset = "custom";
    sendViewerPlaybackProfile(nextProfile, { manualOverride: true });
  }

  function applyViewerStreamTuning() {
    stopViewerAutoUpgradeTimer();
    viewerPreset = "custom";
    sendViewerPlaybackProfile(viewerPlaybackProfile, { manualOverride: true });
  }

  function applyViewerPreset(preset: "low-latency" | "balanced" | "quality") {
    viewerPreset = preset;

    if (preset === "low-latency") {
      viewerPlaybackProfile = "responsive";
      viewerFpsTier = "active";
      viewerBitrateTier = "medium";
    } else if (preset === "balanced") {
      viewerPlaybackProfile = "responsive";
      viewerFpsTier = "normal";
      viewerBitrateTier = "medium";
    } else {
      viewerPlaybackProfile = "quality";
      viewerFpsTier = "active";
      viewerBitrateTier = "good";
    }

    stopViewerAutoUpgradeTimer();
    sendViewerPlaybackProfile(viewerPlaybackProfile, { manualOverride: true });
  }

  function handleViewerVideoFocus() {
    viewerKeyboardCaptured = true;
    revealViewerControls();
  }

  function handleViewerVideoBlur() {
    viewerKeyboardCaptured = false;
  }

  function getViewerPointerPosition(event: MouseEvent) {
    const videoEl = viewerVideoEl;
    if (!videoEl) {
      return null;
    }

    const rect = videoEl.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) {
      return null;
    }

    const scaleX = viewerRemoteWidth / rect.width;
    const scaleY = viewerRemoteHeight / rect.height;
    const x = Math.min(Math.max(Math.round((event.clientX - rect.left) * scaleX), 0), viewerRemoteWidth - 1);
    const y = Math.min(Math.max(Math.round((event.clientY - rect.top) * scaleY), 0), viewerRemoteHeight - 1);

    return { x, y };
  }

  function handleViewerMouseMove(event: MouseEvent) {
    revealViewerControls();

    if (!canSendViewerInput()) {
      return;
    }

    const position = getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    const now = performance.now();
    if (now - lastViewerMouseMoveSentAt < viewerMouseMoveMinIntervalMs) {
      return;
    }

    if (lastViewerPointerSent && lastViewerPointerSent.x === position.x && lastViewerPointerSent.y === position.y) {
      return;
    }

    lastViewerMouseMoveSentAt = now;
    lastViewerPointerSent = position;

    void sendViewerInput({
      type: "mouse-move",
      x: position.x,
      y: position.y
    });
  }

  function handleViewerMouseDown(event: MouseEvent) {
    revealViewerControls();

    if (!canSendViewerInput()) {
      return;
    }

    event.preventDefault();
    viewerVideoEl?.focus();

    const position = getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    void sendViewerInput({
      type: "mouse-down",
      button: event.button,
      x: position.x,
      y: position.y
    });
  }

  function handleViewerMouseUp(event: MouseEvent) {
    revealViewerControls();

    if (!canSendViewerInput()) {
      return;
    }

    event.preventDefault();

    const position = getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    void sendViewerInput({
      type: "mouse-up",
      button: event.button,
      x: position.x,
      y: position.y
    });
  }

  function handleViewerDoubleClick(event: MouseEvent) {
    revealViewerControls();

    if (!canSendViewerInput()) {
      return;
    }

    event.preventDefault();

    const position = getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    void sendViewerInput({
      type: "dblclick",
      button: event.button,
      x: position.x,
      y: position.y
    });
  }

  function handleViewerWheel(event: WheelEvent) {
    revealViewerControls();

    if (!canSendViewerInput()) {
      return;
    }

    const now = performance.now();
    if (now - lastViewerWheelSentAt < viewerWheelMinIntervalMs) {
      event.preventDefault();
      return;
    }
    lastViewerWheelSentAt = now;

    event.preventDefault();
    void sendViewerInput({
      type: "wheel",
      deltaY: event.deltaY
    });
  }

  function isEditableTarget(target: EventTarget | null) {
    if (
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      target instanceof HTMLSelectElement
    ) {
      return true;
    }

    return target instanceof HTMLElement && target.isContentEditable;
  }

  function handleViewerDocumentKeyDown(event: KeyboardEvent) {
    if (!canSendViewerKeyboardInput() || isEditableTarget(event.target)) {
      return;
    }

    event.preventDefault();
    void sendViewerInput({
      type: "key-down",
      key: event.key,
      code: event.code,
      modifiers: {
        ctrl: event.ctrlKey,
        alt: event.altKey,
        shift: event.shiftKey,
        meta: event.metaKey
      }
    });
  }

  function handleViewerDocumentKeyUp(event: KeyboardEvent) {
    if (!canSendViewerKeyboardInput() || isEditableTarget(event.target)) {
      return;
    }

    event.preventDefault();
    void sendViewerInput({
      type: "key-up",
      key: event.key,
      code: event.code
    });
  }

  function resetViewerPeerConnection() {
    diag("resetViewerPeerConnection CALLED");
    console.trace("[DIAG] resetViewerPeerConnection stack");
    bufferedLocalIceCandidates = [];
    stopInboundStatsLogger();
    stopIceConvergenceWatchdog();
    stopIceRestartTimer();
    iceRestartInFlight = false;
    stopViewerOfferRetry();
    stopViewerControlsAutoHide();
    viewerAnswerReceived = false;
    viewerHadConnectedOnce = false;
    viewerSignalProcessing = Promise.resolve();
    pendingViewerIceCandidates = [];
    viewerOfferRetryCount = 0;
    viewerDataChannelOpen = false;
    viewerKeyboardCaptured = false;
    lastViewerMouseMoveSentAt = 0;
    lastViewerWheelSentAt = 0;
    lastViewerPointerSent = null;
    viewerConnectionState = "idle";
    viewerControlsVisible = true;
    viewerRemoteWidth = 1920;
    viewerRemoteHeight = 1080;
    viewerStreamMbps = null;
    viewerStreamFps = null;
    viewerFpsTier = "auto";
    viewerBitrateTier = "auto";
    viewerPreset = "balanced";
    stopViewerAutoUpgradeTimer();
    viewerProfileManualOverride = false;
    viewerPlaybackProfile = "responsive";

    try {
      viewerControlChannel?.close();
    } catch {
      // ignore close errors
    } finally {
      viewerControlChannel = null;
    }

    resetFileChannel();

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

    screenFrameError = null;
  }

  $effect(() => {
    const videoEl = viewerVideoEl;
    const stream = viewerRemoteStream;

    if (!videoEl) {
      return;
    }

    const handleLoadedMetadata = () => {
      diag("video.loadedmetadata", { width: videoEl.videoWidth, height: videoEl.videoHeight });
      syncViewerVideoMetadata(videoEl);
    };
    videoEl.addEventListener("loadedmetadata", handleLoadedMetadata);

    // Extra video lifecycle diagnostics
    const onPlay = () => diag("video.onplay");
    const onPause = () => diag("video.onpause");
    const onWaiting = () => diag("video.onwaiting (no data)");
    const onStalled = () => diag("video.onstalled");
    const onError = () => diag("video.onerror", videoEl.error?.message);
    videoEl.addEventListener("play", onPlay);
    videoEl.addEventListener("pause", onPause);
    videoEl.addEventListener("waiting", onWaiting);
    videoEl.addEventListener("stalled", onStalled);
    videoEl.addEventListener("error", onError);

    if (videoEl.srcObject !== (stream ?? null)) {
      diag("video.srcObject assigned", { hasStream: !!stream, streamId: stream?.id });
      videoEl.srcObject = stream;
    }

    if (stream) {
      syncViewerVideoMetadata(videoEl);
      void videoEl.play().catch(() => {
        // Autoplay may still require a user gesture on some systems.
      });
    }

    return () => {
      videoEl.removeEventListener("loadedmetadata", handleLoadedMetadata);
      videoEl.removeEventListener("play", onPlay);
      videoEl.removeEventListener("pause", onPause);
      videoEl.removeEventListener("waiting", onWaiting);
      videoEl.removeEventListener("stalled", onStalled);
      videoEl.removeEventListener("error", onError);
    };
  });

  function ensureViewerPeerConnection(sessionId: string) {
    if (viewerPeerConnection) {
      return viewerPeerConnection;
    }

    diag("creating new RTCPeerConnection with iceServers", viewerIceServers);
    // Use "all" so the viewer publishes host + srflx + relay candidates.
    // The relay candidates are critical (they survive NAT rebind) and are
    // already embedded in the OFFER via half-trickle ICE.
    const pc = new RTCPeerConnection({
      iceServers: viewerIceServers,
      iceTransportPolicy: "all"
    });

    // Needed to produce an SDP offer even before media integration is complete.
    const inputChannel = pc.createDataChannel("input", { ordered: true });
    configureViewerDataChannel(inputChannel);

    const fileChannelInstance = pc.createDataChannel("file", { ordered: true });
    configureFileDataChannel(fileChannelInstance);

    pc.addTransceiver("video", { direction: "recvonly" });

    viewerConnectionState = pc.connectionState;

    pc.ontrack = (event) => {
      const stream = event.streams?.[0] ?? new MediaStream([event.track]);
      diag("pc.ontrack FIRED", {
        kind: event.track.kind,
        id: event.track.id,
        streamId: stream.id,
        readyState: event.track.readyState,
        muted: event.track.muted,
        enabled: event.track.enabled
      });
      viewerRemoteStream = stream;
      screenFrameError = null;
      applyViewerJitterBufferProfile(pc);

      // Watch the track for "muted" or "ended" events that indicate the
      // browser dropped the stream (codec mismatch, no keyframe, etc.)
      event.track.onmute = () => diag("track.onmute (no media flowing)", { id: event.track.id });
      event.track.onunmute = () => diag("track.onunmute (media resumed)", { id: event.track.id });
      event.track.onended = () => diag("track.onended (track terminated)", { id: event.track.id });
    };

    pc.onconnectionstatechange = () => {
      viewerConnectionState = pc.connectionState;
      diag("pc.connectionState =", pc.connectionState);
      if (pc.connectionState === "connected") {
        viewerHadConnectedOnce = true;
        revealViewerControls();
        screenFrameError = null;
        signalingError = null;
        stopSignalingReconnect();
        stopIceConvergenceWatchdog(); // ICE made it on its own â€” cancel watchdog
        applyViewerJitterBufferProfile(pc);
        maybeAutoUpgradeViewerProfile();
        startInboundStatsLogger(pc);
        stopIceRestartTimer();
      } else if (pc.connectionState === "failed") {
        screenFrameError = "La connexion WebRTC a echoue.";
        stopInboundStatsLogger();
        stopIceConvergenceWatchdog();
        scheduleIceRestart("pc_connection_failed", ICE_RESTART_ON_FAILED_DELAY_MS);
      } else if (pc.connectionState === "closed" || pc.connectionState === "disconnected") {
        stopInboundStatsLogger();
        stopIceConvergenceWatchdog();
        if (pc.connectionState === "disconnected") {
          scheduleIceRestart("pc_connection_disconnected", ICE_RESTART_ON_DISCONNECTED_DELAY_MS);
        }
      }
    };

    pc.oniceconnectionstatechange = () => {
      diag("pc.iceConnectionState =", pc.iceConnectionState);
      if (pc.iceConnectionState === "connected" || pc.iceConnectionState === "completed") {
        stopIceRestartTimer();
        return;
      }
      if (pc.iceConnectionState === "failed") {
        scheduleIceRestart("ice_failed", ICE_RESTART_ON_FAILED_DELAY_MS);
        return;
      }
      if (pc.iceConnectionState === "disconnected") {
        scheduleIceRestart("ice_disconnected", ICE_RESTART_ON_DISCONNECTED_DELAY_MS);
      }
    };

    pc.onicegatheringstatechange = () => {
      diag("pc.iceGatheringState =", pc.iceGatheringState);
    };

    pc.onsignalingstatechange = () => {
      diag("pc.signalingState =", pc.signalingState);
    };

    pc.ondatachannel = (event) => {
      diag("pc.ondatachannel (remote-created)", { label: event.channel.label });
      configureViewerDataChannel(event.channel);
    };

    pc.onicecandidate = (event) => {
      if (!event.candidate) {
        diag("ICE viewer: gathering complete (null candidate)");
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

      // If signaling is closed, buffer the candidate for later flush.
      // CRITICAL: TURN `relay` candidates often arrive AFTER the flaky
      // signaling server has closed with 1011. Without this buffer they are
      // lost, ICE only has host/srflx pairs, and the connection dies in 5-8s.
      if (!signalingConnected) {
        bufferedLocalIceCandidates.push(iceMessage);
        diag("ICE viewer: BUFFERED (signaling closed)", {
          type: event.candidate.type,
          candidate: event.candidate.candidate.slice(0, 80),
          bufferSize: bufferedLocalIceCandidates.length
        });
        return;
      }

      diag("ICE viewer â†’ agent", {
        type: event.candidate.type,
        candidate: event.candidate.candidate.slice(0, 80)
      });

      try {
        signalingClient.send(iceMessage, "viewer");
        logSignal("out", { ...iceMessage, from: "viewer" });
      } catch (err) {
        diag("ICE send to agent FAILED â€” buffering", String(err));
        bufferedLocalIceCandidates.push(iceMessage);
      }
    };

    viewerPeerConnection = pc;
    return pc;
  }

  function sendViewerOfferPayload(
    sessionId: string,
    offer: Pick<RTCSessionDescriptionInit, "type" | "sdp">
  ) {
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

  function startViewerOfferRetry(
    sessionId: string,
    offer: Pick<RTCSessionDescriptionInit, "type" | "sdp">
  ) {
    stopViewerOfferRetry();
    viewerOfferRetryCount = 0;

    viewerOfferRetryTimer = setInterval(() => {
      if (viewerAnswerReceived || viewerPeerConnection?.connectionState === "connected") {
        stopViewerOfferRetry();
        return;
      }

      if (!signalingClient.isConnected()) {
        return;
      }

      if (viewerOfferRetryCount >= maxViewerOfferRetries) {
        stopViewerOfferRetry();
        if (!viewerAnswerReceived) {
          screenFrameError = "Aucune reponse SDP recue. Le viewer a cesse de renvoyer l'offre.";
        }
        return;
      }

      viewerOfferRetryCount += 1;

      try {
        sendViewerOfferPayload(sessionId, offer);
      } catch {
        // ignore transient signaling send issues
      }
    }, 1000);
  }

  /**
   * Wait until the RTCPeerConnection has finished gathering ICE candidates,
   * or until `timeoutMs` elapses (whichever comes first). Used to switch from
   * trickle ICE to half-trickle: by the time we send the OFFER, all local
   * candidates (including TURN `relay` ones) are embedded in the SDP, so we
   * don't depend on the signaling socket staying alive long enough to trickle
   * them individually.
   */
  function waitForIceGathering(pc: RTCPeerConnection, timeoutMs: number): Promise<void> {
    if (pc.iceGatheringState === "complete") {
      return Promise.resolve();
    }
    return new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        pc.removeEventListener("icegatheringstatechange", onChange);
        diag("waitForIceGathering: TIMEOUT", { gatheringState: pc.iceGatheringState });
        resolve();
      }, timeoutMs);
      const onChange = () => {
        if (pc.iceGatheringState === "complete") {
          clearTimeout(timer);
          pc.removeEventListener("icegatheringstatechange", onChange);
          diag("waitForIceGathering: COMPLETE");
          resolve();
        }
      };
      pc.addEventListener("icegatheringstatechange", onChange);
    });
  }

  async function sendViewerOffer(sessionId: string) {
    const pc = ensureViewerPeerConnection(sessionId);
    const offer = await pc.createOffer({ offerToReceiveVideo: true });
    await pc.setLocalDescription(offer);

    // CRITICAL â€” half-trickle ICE: wait for all local candidates (including
    // TURN relay) to be gathered before sending the OFFER. The flaky signaling
    // server tends to close with 1011 mid-trickle, dropping our `relay`
    // candidates. Embedding them directly in the OFFER SDP avoids that.
    // Cap at 4s so we don't block forever if a STUN/TURN server hangs.
    await waitForIceGathering(pc, 4000);

    // Use the FINAL local description (now with all candidates embedded).
    const finalSdp = pc.localDescription?.sdp ?? offer.sdp ?? "";
    const finalType = pc.localDescription?.type ?? offer.type;
    const finalOffer = { type: finalType, sdp: finalSdp };

    // Log codec + candidate counts for visibility.
    const offerH264 = finalSdp.split("\n").filter((l) => /H264|h264/i.test(l));
    const candidateLines = finalSdp.split("\n").filter((l) => l.startsWith("a=candidate"));
    const relayCount = candidateLines.filter((l) => / typ relay/.test(l)).length;
    diag("OFFER created â€” H264 lines", offerH264);
    diag("OFFER candidates embedded", {
      total: candidateLines.length,
      relay: relayCount,
      gatheringState: pc.iceGatheringState
    });
    diag("OFFER recvonly transceivers", pc.getTransceivers().map((t) => ({
      direction: t.direction,
      currentDirection: t.currentDirection,
      kind: t.receiver?.track?.kind
    })));
    viewerAnswerReceived = false;
    screenFrameError = null;

    sendViewerOfferPayload(sessionId, finalOffer);
    startViewerOfferRetry(sessionId, finalOffer);
  }

  async function handleIncomingSignal(message: SignalMessage) {
    if (message.type === "LEAVE") {
      signalingRemoteEnded = true;
      signalingManualDisconnect = true;
      stopSignalingReconnect();
      stopViewerOfferRetry();
      signalingError = "Session terminee par le poste distant.";
      screenFrameError = "Le poste distant a ferme la session.";
      signalingClient.disconnect();
      clearSignalingListeners();
      resetViewerPeerConnection();
      disconnectChat(); // session ended remotely â€” tear down STOMP too
      signalingConnected = false;
      if (backendSessionSynced) {
        await leaveBackendSession();
      } else {
        backendSyncError = null;
      }
      return;
    }

    if (message.type === "ERROR") {
      const payload = message.payload as Record<string, unknown> | null;
      const reason =
        (typeof payload?.error === "string" && payload.error) ||
        (typeof payload?.message === "string" && payload.message) ||
        "Erreur signaling recue depuis l'agent.";
      screenFrameError = reason;
      return;
    }

    if (message.type === "STREAM_STATS") {
      const payload = message.payload as Record<string, unknown> | null;
      viewerStreamMbps = Number(payload?.mbps ?? 0);
      viewerStreamFps = Number(payload?.fps ?? 0);
      maybeAutoUpgradeViewerProfile();
      return;
    }

    if (message.type === "ANSWER") {
      const payload = message.payload as { type?: string; sdp?: string } | null;
      if (!payload?.sdp || !payload?.type) {
        diag("ANSWER ignored â€” empty payload", payload);
        return;
      }

      const pc = viewerPeerConnection;
      if (!pc) {
        diag("ANSWER ignored â€” no viewerPeerConnection");
        return;
      }

      viewerAnswerReceived = true;
      stopViewerOfferRetry();
      screenFrameError = null;

      // Inspect codec lines so we can compare with what the agent negotiated.
      const h264Lines = payload.sdp.split("\n").filter((l) => /H264|h264/i.test(l));
      diag("ANSWER received â€” H264 lines in SDP", h264Lines);

      try {
        await pc.setRemoteDescription({
          type: payload.type as RTCSdpType,
          sdp: payload.sdp
        });
        diag("setRemoteDescription OK", { signalingState: pc.signalingState });
        stopIceRestartTimer();
        iceRestartInFlight = false;
      } catch (err) {
        diag("setRemoteDescription FAILED", String(err));
        screenFrameError = `setRemoteDescription failed: ${String(err)}`;
        return;
      }

      sendViewerPlaybackProfile(viewerPlaybackProfile);
      maybeAutoUpgradeViewerProfile();

      if (pendingViewerIceCandidates.length > 0) {
        const queued = pendingViewerIceCandidates;
        pendingViewerIceCandidates = [];
        diag("draining queued ICE candidates", { count: queued.length });
        for (const candidate of queued) {
          try {
            await pc.addIceCandidate(candidate);
          } catch (error) {
            diag("queued addIceCandidate FAILED", String(error));
          }
        }
      }
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

      const candidateInit: RTCIceCandidateInit = {
        candidate: payload.candidate,
        sdpMid: payload.sdpMid ?? null,
        sdpMLineIndex: payload.sdpMLineIndex ?? null
      };

      // Parse the candidate type so we can see if we're getting host / srflx /
      // relay candidates from the agent. If only host/srflx (no relay), TURN
      // is missing on the agent side and ICE will fail across NAT.
      const candStr = candidateInit.candidate ?? "";
      const typMatch = candStr.match(/typ (\w+)/);
      const candType = typMatch ? typMatch[1] : "?";
      diag("ICE from agent", { type: candType, candidate: candStr.slice(0, 80) });

      const pc = viewerPeerConnection;
      if (!pc.remoteDescription) {
        pendingViewerIceCandidates.push(candidateInit);
        diag("ICE queued (no remoteDescription yet)", { queueLen: pendingViewerIceCandidates.length });
        return;
      }

      try {
        await pc.addIceCandidate(candidateInit);
      } catch (error) {
        diag("addIceCandidate FAILED", String(error));
      }
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

  function shouldBridgeSessionToLocalAgent(session: ControlSession) {
    const targetMachineId = session.agentMachineId?.trim() ?? "";
    const currentMachineId = localMachineId.trim();
    return !!targetMachineId && !!currentMachineId && targetMachineId === currentMachineId;
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

  function toggleMetricsPanel() {
    metricsPanelOpen = !metricsPanelOpen;
  }

  function syncViewerFullscreenState() {
    viewerFullscreenActive = !!viewerShellEl && document.fullscreenElement === viewerShellEl;
  }

  async function toggleViewerFullscreen() {
    if (!viewerShellEl) {
      return;
    }

    if (document.fullscreenElement === viewerShellEl) {
      await document.exitFullscreen();
      return;
    }

    await viewerShellEl.requestFullscreen();
  }

  async function enterViewerFullscreen() {
    if (!viewerShellEl || document.fullscreenElement === viewerShellEl) return;
    try { await viewerShellEl.requestFullscreen(); } catch { /* user gesture / API absente */ }
  }

  async function exitViewerFullscreen() {
    if (document.fullscreenElement) {
      try { await document.exitFullscreen(); } catch { /* déjà sorti */ }
    }
  }

  // ── Contrôle d'émission de frames côté agent (Pause / Play / transfert) ──
  // L'agent démarre PAUSÉ. On lui envoie VIDEO_RESUME / VIDEO_PAUSE via le
  // DataChannel "input" (P2P, reste ouvert même quand le signaling WebSocket
  // est fermé par Render avec un 1003 après l'OFFER/ANSWER).
  let rdFileInputEl = $state<HTMLInputElement | null>(null);
  let rdVideoPausedForTransfer = $state(false);
  // Préférence utilisateur : true = on VEUT voir l'écran (Play). Combiné avec
  // l'état des transferts, ça donne l'état réel envoyé à l'agent.
  let rdScreenPlayRequested = $state(false);

  function rdHasActiveTransfer(transfers: Record<string, FileTransfer>): boolean {
    for (const t of Object.values(transfers)) {
      if (t.state === "active") return true;
    }
    return false;
  }

  function rdSendVideoControl(paused: boolean) {
    const payload = JSON.stringify({ type: paused ? "VIDEO_PAUSE" : "VIDEO_RESUME" });
    // 1) Voie principale : DataChannel "input" (P2P, toujours ouvert si peer Connected)
    try {
      if (viewerControlChannel && viewerControlChannel.readyState === "open") {
        viewerControlChannel.send(payload);
      }
    } catch {
      /* canal momentanément KO */
    }
    // 2) Voie de secours : signaling, si encore connecté (utile au tout début
    //    avant l'ouverture du DataChannel input).
    try {
      if (signalingClient.isConnected()) {
        signalingClient.send({
          type: "STREAM_PROFILE",
          to: "agent",
          sessionId: activeSession ? String(activeSession.id) : undefined,
          payload: { profile: viewerPlaybackProfile, paused }
        }, "viewer");
      }
    } catch {
      /* ignore */
    }
  }

  function rdPlayScreen() {
    rdScreenPlayRequested = true;
  }
  function rdPauseScreen() {
    rdScreenPlayRequested = false;
  }

  // Quand l'utilisateur ouvre la carte "Écran" pour la PREMIÈRE FOIS dans une
  // session, on arme automatiquement le Play (sinon il faudrait deux clicks
  // pour démarrer la diffusion). Une fois cliqué Pause, le souhait utilisateur
  // est respecté (rdAutoPlayDoneForSession évite que l'effet re-set true).
  let rdAutoPlayDoneForSession = $state<number | null>(null);
  $effect(() => {
    const sid = activeSession?.id ?? null;
    const isActive = activeSession?.status === "ACTIVE";
    const onScreen = selectedFeature === "screen";
    if (sid && isActive && onScreen && rdAutoPlayDoneForSession !== sid) {
      rdScreenPlayRequested = true;
      rdAutoPlayDoneForSession = sid;
    }
  });

  // Reflète l'état désiré en envoi P2P. Pause si :
  //  - l'utilisateur n'a pas (encore) cliqué Play, OU
  //  - un transfert est en cours, OU
  //  - on n'est pas sur le panneau Écran
  $effect(() => {
    // Dépendances réactives explicites — y compris viewerDataChannelOpen
    // pour que l'effet se redéclenche quand le canal devient utilisable
    // (et qu'on puisse ENFIN envoyer le PAUSE/RESUME désiré).
    void viewerDataChannelOpen;

    const transferActive = rdHasActiveTransfer(fileTransfers);
    rdVideoPausedForTransfer = transferActive;

    const onScreenTab = selectedFeature === "screen";
    const wantFrames = onScreenTab && rdScreenPlayRequested && !transferActive;
    const paused = !wantFrames;

    // Évite de spam : on track la dernière valeur envoyée
    if (paused !== rdLastSentPaused) {
      rdSendVideoControl(paused);
      rdLastSentPaused = paused;
    }
  });
  let rdLastSentPaused: boolean | null = null;

  // Réinitialise les états quand la session se ferme
  $effect(() => {
    if (!activeSession) {
      rdScreenPlayRequested = false;
      rdLastSentPaused = null;
      rdAutoPlayDoneForSession = null;
    }
  });

  function rdTriggerFilePicker() {
    // Source de vérité: l'état réel du canal au moment du clic, pas le state Svelte
    if (fileChannel?.readyState !== "open") {
      console.warn("[file-ch] picker triggered but channel is", fileChannel?.readyState);
      return;
    }
    rdFileInputEl?.click();
  }

  async function rdHandleFilePicked(event: Event) {
    const input = event.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) return;
    for (const file of Array.from(files)) {
      try {
        await uploadLocalFile(file);
      } catch (err) {
        console.error("[rd] upload failed:", err);
      }
    }
    // Reset pour pouvoir re-sélectionner le même fichier plus tard
    input.value = "";
  }

  function rdDismissTransfer(tid: string) {
    const next = { ...fileTransfers };
    delete next[tid];
    fileTransfers = next;
  }

  function rdProgressPercent(t: FileTransfer): number {
    if (t.totalSize <= 0) return 0;
    return Math.min(100, Math.round((t.doneBytes / t.totalSize) * 100));
  }

  // Polling fiable de l'état du DataChannel "file" — Svelte ne peut pas
  // observer fileChannel.readyState directement, donc on en fait un miroir
  // dans un $state régénéré à 500 ms.
  let rdFileChannelLive = $state(false);
  $effect(() => {
    const tick = () => {
      const open = fileChannel?.readyState === "open";
      if (open !== rdFileChannelLive) rdFileChannelLive = open;
    };
    tick();
    const id = setInterval(tick, 500);
    return () => clearInterval(id);
  });

  function toggleViewerExpanded() {
    viewerExpanded = !viewerExpanded;
    revealViewerControls();
  }

  async function connectSignaling(options?: { force?: boolean; reason?: string }) {
    const forceConnect = options?.force === true;
    const forceReason = options?.reason ?? null;
    diag("connectSignaling CALLED", {
      alreadyConnected: signalingConnected,
      inFlight: connectSignalingInFlight,
      force: forceConnect,
      reason: forceReason
    });
    console.trace("[DIAG] connectSignaling stack");

    if (signalingConnected) {
      diag("connectSignaling SKIPPED â€” already connected");
      return;
    }
    if (connectSignalingInFlight) {
      // Race guard: another connect attempt is already opening a WebSocket.
      // Without this, two parallel connects with the same token cause the
      // server to reject one of them with 1003.
      diag("connectSignaling SKIPPED â€” already in flight (race guard)");
      return;
    }
    connectSignalingInFlight = true;

    const current = queriedSession ?? activeSession;
    if (!current) {
      signalingError = "Demarrez ou chargez une session avant la connexion signaling.";
      connectSignalingInFlight = false;
      return;
    }

    // Do not churn signaling sockets while media is already healthy.
    // Reconnect is only needed on-demand (e.g. ICE restart when media drops).
    const peerState = viewerPeerConnection?.connectionState;
    if (!forceConnect && peerState === "connected") {
      diag("connectSignaling SKIPPED â€” peer already connected (background reconnect disabled)");
      connectSignalingInFlight = false;
      return;
    }

    diag("connectSignaling using session", {
      id: current.id,
      status: current.status,
      tokenSuffix: current.signalingToken?.slice(-8)
    });

    signalingError = null;
    signalLogs = [];
    viewerStreamMbps = null;
    viewerStreamFps = null;
    stopViewerAutoUpgradeTimer();
    stopSignalingReconnect();
    viewerProfileManualOverride = false;
    viewerPlaybackProfile = "responsive";
    revealViewerControls();
    signalingManualDisconnect = false;
    signalingRemoteEnded = false;

    try {
      await signalingClient.connect(current.signalingToken, "viewer", String(current.id));
      signalingReconnectAttempts = 0;
      if (shouldBridgeSessionToLocalAgent(current)) {
        try {
          await joinBackendSession(current);
        } catch (error) {
          backendSessionSynced = false;
          backendSyncError = String(error);
        }
      } else {
        backendSessionSynced = false;
        backendSyncError = null;
      }
      clearSignalingListeners();

      detachMessageListener = signalingClient.onMessage((message) => {
        logSignal("in", message);
        viewerSignalProcessing = viewerSignalProcessing
          .then(() => handleIncomingSignal(message))
          .catch((error) => {
            if (uiDebugEnabled) {
              console.error("Signal processing failed", error);
            }
          });
      });

      detachCloseListener = signalingClient.onClose((event) => {
        const closeCode = event.code ?? 0;
        const peerState = viewerPeerConnection?.connectionState;
        const iceState = viewerPeerConnection?.iceConnectionState;
        diag("signaling SOCKET CLOSED", {
          code: closeCode,
          reason: event.reason,
          wasClean: event.wasClean,
          manualDisconnect: signalingManualDisconnect,
          remoteEnded: signalingRemoteEnded,
          peerState,
          iceState
        });
        signalingConnected = false;

        const isManualEnd = signalingManualDisconnect || signalingRemoteEnded;
        const peerTerminal =
          peerState === "failed" || peerState === "closed";
        const peerAlreadyConnected = peerState === "connected";

        // Signaling can flap while media is still alive. Keep the peer when
        // possible and reconnect signaling in background so ICE restart remains
        // available if connectivity degrades later.

        if (isManualEnd || peerTerminal) {
          diag("signaling close â†’ RESETTING peer", { isManualEnd, peerTerminal });
          stopSignalingReconnect();
          resetViewerPeerConnection();
          if (closeCode === 1003) {
            signalingError = "Signal ferme (1003): session/token invalide ou expire. Recharge la session.";
          } else if (closeCode === 1000) {
            signalingError = "Signal ferme normalement (1000).";
          } else {
            signalingError = `Signal ferme (code ${closeCode}).`;
          }
          if (backendSessionSynced) {
            void leaveBackendSession();
          }
          return;
        }        if (peerAlreadyConnected) {
          diag("signaling close â†’ peer CONNECTED, keep media and reconnect signaling in background");
          signalingError = null;
          // Keep media alive AND keep signaling reconnecting in the background:
          // we still need it for ICE restart, future OFFERs, chat and stats.
          // Reset the attempt counter so the backoff starts fresh each time
          // the peer is healthy (otherwise it drifts to the 10s ceiling).
          signalingReconnectAttempts = 0;
          scheduleSignalingReconnect();
          return;
        }

        // Peer still negotiating (`new` / `connecting` / `checking`). Give it
        // a grace window to converge using the ICE candidates already on the
        // wire. If ICE doesn't reach `connected` in time, declare failure.
        diag("signaling close â†’ giving ICE a grace window to converge", {
          peerState,
          iceState
        });        signalingError = "Signal perdu — tentative de reprise signaling et ICE...";
        // Always keep retrying — previous logic stopped after the first
        // signaling drop once the peer had ever been connected, leaving
        // the viewer stranded with no way to reach the agent again.
        scheduleSignalingReconnect();
        startIceConvergenceWatchdog();
      });

      detachErrorListener = signalingClient.onError(() => {
        signalingError = "Erreur socket signaling";
      });

      signalingConnected = true;

      // Flush any ICE candidates that were generated while signaling was down
      // (often the critical TURN `relay` candidates that arrive late).
      if (bufferedLocalIceCandidates.length > 0) {
        diag("flushing buffered ICE candidates", { count: bufferedLocalIceCandidates.length });
        const toFlush = bufferedLocalIceCandidates;
        bufferedLocalIceCandidates = [];
        for (const ice of toFlush) {
          try {
            signalingClient.send(ice, "viewer");
          } catch (err) {
            diag("flush ICE FAILED â€” re-buffering", String(err));
            bufferedLocalIceCandidates.push(ice);
          }
        }
      }

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

      // CRITICAL: only send a fresh OFFER if we don't already have a working
      // peer. After a transient signaling close (1006/1011/1012/1013) the peer
      // is still alive and re-OFFERing would consume the token a second time
      // â†’ the server replies with 1003.
      const existingPeerState = viewerPeerConnection?.connectionState;
      const peerAlreadyAlive =
        !!viewerPeerConnection &&
        existingPeerState !== "closed" &&
        existingPeerState !== "failed";

      if (peerAlreadyAlive) {
        diag("connectSignaling: peer already alive â€” skipping re-OFFER", {
          peerState: existingPeerState
        });
      } else {
        await sendViewerOffer(String(current.id));
      }
    } catch (error) {
      diag("connectSignaling THREW", String(error));
      signalingClient.disconnect();
      signalingConnected = false;
      const peerState = viewerPeerConnection?.connectionState;
      const peerAlive =
        peerState === "connected" ||
        peerState === "connecting" ||
        peerState === "disconnected";
      if (!peerAlive || signalingManualDisconnect) {
        resetViewerPeerConnection();
      } else {
        diag("connectSignaling failed but peer is still alive — keeping peer", { peerState });
      }
      if (signalingManualDisconnect) {
        backendSessionSynced = false;
        backendSyncError = null;
      }
      signalingError = String(error);

      if (!signalingManualDisconnect) {
        // Always keep retrying so the signaling channel comes back even
        // after the peer has been healthy at some point — needed for
        // ICE restart / chat / stats if connectivity later degrades.
        scheduleSignalingReconnect();
      }
    } finally {
      // Always release the in-flight lock so the next legitimate connect attempt
      // (e.g. scheduled reconnect) can proceed.
      connectSignalingInFlight = false;
    }
  }

  async function disconnectSignaling(options?: { sendLeave?: boolean }) {
    diag("disconnectSignaling CALLED", { sendLeave: options?.sendLeave === true });
    // Stack trace so we see exactly which caller fired this â€” Svelte effect,
    // onDestroy, button click, error handler, etc.
    console.trace("[DIAG] disconnectSignaling stack");

    signalingManualDisconnect = true;
    stopSignalingReconnect();

    const shouldSendLeave = options?.sendLeave === true;
    const current = queriedSession ?? activeSession;
    if (shouldSendLeave && signalingClient.isConnected() && current?.id) {
      try {
        const leaveMessage: SignalMessage = {
          type: "LEAVE",
          to: "agent",
          sessionId: String(current.id),
          payload: {
            role: "viewer",
            reason: "manual_disconnect"
          }
        };
        diag("SENDING LEAVE", leaveMessage);
        signalingClient.send(leaveMessage, "viewer");
      } catch (err) {
        diag("LEAVE send failed", String(err));
      }
    }

    signalingClient.disconnect();
    resetViewerPeerConnection();
    clearSignalingListeners();
    signalingConnected = false;
    signalingReconnectAttempts = 0;
    if (backendSessionSynced) {
      await leaveBackendSession();
    } else {
      backendSyncError = null;
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
    const handleKeyDown = (event: KeyboardEvent) => handleViewerDocumentKeyDown(event);
    const handleKeyUp = (event: KeyboardEvent) => handleViewerDocumentKeyUp(event);
    const handleFullscreenChange = () => syncViewerFullscreenState();
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("keyup", handleKeyUp);
    document.addEventListener("fullscreenchange", handleFullscreenChange);
    detachViewerInputListeners = () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("keyup", handleKeyUp);
    };
    detachViewerFullscreenListener = () => {
      document.removeEventListener("fullscreenchange", handleFullscreenChange);
    };

    void syncAgentLifecycle();
    void loadLocalMachineId();
    void refreshViewerIceServers();
    refreshMetrics();
    refreshOnlineAgents();
    void checkPendingApproval();

    metricsTimer = setInterval(refreshMetrics, 2500);
    agentsTimer = setInterval(refreshOnlineAgents, 8000);
    approvalTimer = setInterval(checkPendingApproval, 3000);
  });

  // Track real app shutdown (window close, page unload). Set ONLY when the
  // browser/Tauri actually fires beforeunload / pagehide. Any other onDestroy
  // (Svelte rerender, HMR, route swap) must NOT tear down the session.
  let realUnloadInProgress = false;
  if (typeof window !== "undefined") {
    const markRealUnload = () => {
      realUnloadInProgress = true;
      diag("real unload event fired (window closing)");
    };
    window.addEventListener("beforeunload", markRealUnload);
    window.addEventListener("pagehide", markRealUnload);
  }

  onDestroy(() => {
    diag("onDestroy fired", {
      realUnload: realUnloadInProgress,
      visibility: typeof document !== "undefined" ? document.visibilityState : "n/a",
      hmrAvailable: typeof (import.meta as unknown as { hot?: unknown }).hot !== "undefined"
    });

    // Visual timers + DOM listeners can always be cleaned (no network impact).
    detachViewerInputListeners?.();
    detachViewerInputListeners = null;
    detachViewerFullscreenListener?.();
    detachViewerFullscreenListener = null;
    clearInterval(metricsTimer);
    clearInterval(agentsTimer);
    if (approvalTimer) clearInterval(approvalTimer);
    if (typingClearTimer) clearTimeout(typingClearTimer);

    // CRITICAL: only tear down the network sessions if the user is REALLY
    // leaving the app (window/tab close). Otherwise, a Svelte rerender,
    // route change, HMR, or component unmount would kill the active video
    // session â€” which is what was happening to you.
    if (!realUnloadInProgress) {
      diag("onDestroy SKIPPED network teardown â€” not a real unload");
      return;
    }

    diag("onDestroy: real teardown (window closing)");
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

<main class="rd-page">
  <!-- ═════════════════════════════════════════════════════════════════
       Nouvelle UI "Bureau à Distance" (cf. maquette).
       L'ancienne console technicien est conservée plus bas dans un
       bloc {#if false}…{/if} (équivalent d'un commentaire de bloc Svelte).
       ═════════════════════════════════════════════════════════════════ -->
  <section class="rd-card">
    <header class="rd-header">
      <div class="rd-title">
        <h1>Bureau à Distance</h1>
        <p>Contrôlez et partagez votre bureau en toute sécurité</p>
      </div>
      <div class="rd-machine-code">
        <span class="rd-machine-code__label">Code de cette machine</span>
        <span class="rd-machine-code__value">
          {#if localConnectionCodeLoading && !localConnectionCode}
            …
          {:else if localConnectionCode}
            {localConnectionCode}
          {:else}
            ------
          {/if}
        </span>
      </div>
    </header>

    <section class="rd-panel">
      <h2 class="rd-panel__title">Connexion par code</h2>
      <div class="rd-connect">
        <input
          class="rd-connect__input"
          type="text"
          placeholder="Entrez le code de l'ordinateur distant"
          bind:value={connectionCode}
          disabled={actionLoading || waitingForApproval}
          onkeydown={(e) => { if (e.key === "Enter" && !actionLoading) void startSessionWithCode(); }} />
        <button
          class="rd-connect__btn"
          type="button"
          onclick={() => void startSessionWithCode()}
          disabled={actionLoading || waitingForApproval || !connectionCode.trim()}>
          {actionLoading ? "Connexion…" : "Se connecter →"}
        </button>
      </div>

      {#if waitingForApproval}
        <div class="rd-connect__status rd-connect__status--waiting">
          <span class="rd-spinner"></span>
          <div>
            <strong>En attente d'acceptation…</strong>
            <p>L'ordinateur distant doit autoriser la connexion (clavier, souris, transfert de fichiers).</p>
          </div>
        </div>
      {:else if actionError}
        <div class="rd-connect__status rd-connect__status--error">
          <strong>Erreur :</strong> {actionError}
        </div>
      {/if}
    </section>

    <!-- ── Session active : menu Écran / Fichier / Chat ─────────────── -->
    {#if activeSession && activeSession.status === "ACTIVE" && !selectedFeature}
      <section class="rd-panel">
        <header class="rd-session-menu__head">
          <div>
            <h2 class="rd-panel__title">
              <span class="rd-icon">🔗</span>
              Session établie avec
              <strong class="rd-viewer__peer">{activeSession.agentMachineId}</strong>
            </h2>
            <p class="rd-viewer__sub">Choisis quelle fonctionnalité utiliser. La vidéo ne démarre que si tu cliques "Écran".</p>
          </div>
          <button
            class="rd-viewer__disconnect"
            type="button"
            onclick={() => void stopByToken()}
            disabled={actionLoading}>
            Déconnecter
          </button>
        </header>

        <div class="rd-features">
          <button class="rd-feature" type="button" onclick={() => { selectedFeature = "screen"; }}>
            <span class="rd-feature__icon">🖥</span>
            <strong>Écran</strong>
            <span class="rd-feature__hint">Voir et contrôler le bureau distant</span>
          </button>
          <button class="rd-feature" type="button" onclick={() => { selectedFeature = "files"; }}>
            <span class="rd-feature__icon">📄</span>
            <strong>Transfert de fichiers</strong>
            <span class="rd-feature__hint">Envoyer/recevoir sans afficher l'écran</span>
          </button>
          <button class="rd-feature" type="button" onclick={() => { selectedFeature = "chat"; }}>
            <span class="rd-feature__icon">💬</span>
            <strong>Chat</strong>
            <span class="rd-feature__hint">Échanger des messages</span>
          </button>
        </div>
      </section>
    {/if}

    <!-- ── Sous-panneau "Transfert de fichiers" (sans écran) ─────────── -->
    {#if activeSession && activeSession.status === "ACTIVE" && selectedFeature === "files"}
      <section class="rd-panel">
        <header class="rd-session-menu__head">
          <div>
            <h2 class="rd-panel__title"><span class="rd-icon">📄</span> Transfert de fichiers</h2>
            <p class="rd-viewer__sub">
              Vidéo désactivée — toute la bande passante est dédiée au transfert.
              {#if rdFileChannelLive}<span style="color:#4ade80">● Canal P2P ouvert</span>{:else}<span style="color:#fbbf24">● Canal en attente…</span>{/if}
            </p>
          </div>
          <div class="rd-viewer__actions">
            <button class="rd-viewer__btn" type="button" onclick={() => { selectedFeature = null; }}>← Menu</button>
            <button
              class="rd-viewer__btn"
              type="button"
              onclick={rdTriggerFilePicker}
              disabled={!rdFileChannelLive}>📤 Envoyer fichier</button>
            <button
              class="rd-viewer__disconnect"
              type="button"
              onclick={() => void stopByToken()}
              disabled={actionLoading}>Déconnecter</button>
          </div>
        </header>
        <input bind:this={rdFileInputEl} type="file" multiple style="display:none" onchange={rdHandleFilePicked} />
        {#if Object.keys(fileTransfers).length === 0}
          <p class="rd-empty">Aucun transfert pour l'instant. Clique "Envoyer fichier" pour démarrer.</p>
        {:else}
          <div class="rd-transfers" style="margin-top: 0;">
            {#each Object.values(fileTransfers).sort((a, b) => b.startedAt - a.startedAt) as t (t.transferId)}
              <article class="rd-transfer rd-transfer--{t.state}">
                <div class="rd-transfer__icon">{t.type === "upload" ? "📤" : "📥"}</div>
                <div class="rd-transfer__body">
                  <div class="rd-transfer__line">
                    <strong class="rd-transfer__name">{t.fileName}</strong>
                    <span class="rd-transfer__meta">
                      {rdFormatBytes(t.doneBytes)} / {rdFormatBytes(t.totalSize)}
                      {#if t.state === "active"}&nbsp;•&nbsp; {rdProgressPercent(t)}%{/if}
                    </span>
                  </div>
                  <div class="rd-transfer__bar">
                    <div class="rd-transfer__bar-fill" style="width: {rdProgressPercent(t)}%"></div>
                  </div>
                  {#if t.state === "error"}<p class="rd-transfer__error">Erreur : {t.error ?? "inconnue"}</p>{/if}
                  {#if t.state === "complete"}
                    {#if t.type === "upload"}
                      {#if t.destPath}
                        <p class="rd-transfer__done">
                          ✓ Envoyé à <strong>{activeSession?.agentMachineId ?? "l'autre PC"}</strong>
                          ({(t.totalSize / 1024).toFixed(1)} KB)
                        </p>
                        <p class="rd-transfer__where">
                          📁 <strong>Le fichier est sur l'AUTRE ordinateur</strong>
                          ({activeSession?.agentMachineId ?? "?"}), pas sur celui-ci.<br />
                          Sur le PC distant, ouvre le dossier
                          <strong>Téléchargements</strong> →
                          <strong>LumiereTransfers</strong>
                          (sous-dossier créé automatiquement).
                          <br />
                          Chemin complet :
                          <code style="user-select:all">{t.destPath}</code>
                          <button
                            class="rd-transfer__copy"
                            type="button"
                            onclick={() => navigator.clipboard?.writeText(t.destPath ?? "")}
                            title="Copier le chemin">📋 Copier</button>
                        </p>
                      {:else}
                        <p class="rd-transfer__done rd-transfer__done--warn">
                          ⚠ Tous les chunks envoyés mais pas d'ACK reçu de l'autre PC.
                          L'agent distant ne confirme pas la réception — vérifie sa console
                          (logs <code>[file-ch]</code>).
                        </p>
                      {/if}
                    {:else}
                      <p class="rd-transfer__done">✓ Reçu et téléchargé localement</p>
                    {/if}
                  {/if}
                </div>
                {#if t.state !== "active"}
                  <button class="rd-transfer__close" type="button" onclick={() => rdDismissTransfer(t.transferId)}>×</button>
                {/if}
              </article>
            {/each}
          </div>
        {/if}
      </section>
    {/if}

    <!-- ── Sous-panneau "Chat" (sans écran) ────────────────────────── -->
    {#if activeSession && activeSession.status === "ACTIVE" && selectedFeature === "chat"}
      <section class="rd-panel">
        <header class="rd-session-menu__head">
          <div>
            <h2 class="rd-panel__title"><span class="rd-icon">💬</span> Chat</h2>
            <p class="rd-viewer__sub">Vidéo désactivée pour économiser la bande passante.</p>
          </div>
          <div class="rd-viewer__actions">
            <button class="rd-viewer__btn" type="button" onclick={() => { selectedFeature = null; }}>← Menu</button>
            <button class="rd-viewer__disconnect" type="button" onclick={() => void stopByToken()} disabled={actionLoading}>Déconnecter</button>
          </div>
        </header>
        <p class="rd-empty">Module chat — utilise le canal STOMP existant (WIP).</p>
      </section>
    {/if}

    <!-- ── Sous-panneau "Écran" : vidéo + Play/Pause ──────────────── -->
    {#if activeSession && activeSession.status === "ACTIVE" && selectedFeature === "screen"}
      <section class="rd-panel rd-viewer">
        <header class="rd-viewer__head">
          <h2 class="rd-panel__title">
            <span class="rd-icon">🖥</span>
            Session en cours avec
            <strong class="rd-viewer__peer">{activeSession.agentMachineId}</strong>
          </h2>
          <p class="rd-viewer__sub">
            {#if signalingConnected && viewerRemoteStream}
              Stream actif
              {#if viewerStreamMbps !== null}&nbsp;•&nbsp; {viewerStreamMbps.toFixed(1)} Mbps{/if}
              {#if viewerStreamFps !== null}&nbsp;•&nbsp; {viewerStreamFps.toFixed(0)} fps{/if}
            {:else if signalingConnected}
              Signalisation connectée — attente de la première image…
            {:else}
              Connexion en cours…
            {/if}
          </p>
        </header>

        <div
          bind:this={viewerShellEl}
          class="rd-viewer__stage"
          class:rd-viewer__stage--ready={!!viewerRemoteStream}
          class:rd-viewer__stage--fullscreen={viewerFullscreenActive}
          onmousemove={revealViewerControls}
          onmouseleave={() => { /* laisse l'auto-hide existant gérer le fade */ }}
          role="presentation">
          {#if viewerRemoteStream}
            <video
              class="rd-viewer__video"
              class:active={canSendViewerInput()}
              bind:this={viewerVideoEl}
              autoplay
              playsinline
              muted
              tabindex="0"
              onfocus={handleViewerVideoFocus}
              onblur={handleViewerVideoBlur}
              onmousemove={handleViewerMouseMove}
              onmousedown={handleViewerMouseDown}
              onmouseup={handleViewerMouseUp}
              onwheel={handleViewerWheel}
              oncontextmenu={(event) => event.preventDefault()}
            ></video>
          {:else}
            <div class="rd-viewer__placeholder">
              <span class="rd-spinner"></span>
              <p>Réception de la première image WebRTC…</p>
            </div>
          {/if}

          <!-- Overlay "transfert en cours" pendant que les frames sont coupées -->
          {#if rdVideoPausedForTransfer}
            <div class="rd-viewer__transfer-overlay">
              <span class="rd-spinner"></span>
              <p>Transfert de fichier en cours — émission de frames suspendue côté agent.</p>
            </div>
          {:else if !rdScreenPlayRequested}
            <div class="rd-viewer__transfer-overlay">
              <button class="rd-viewer__big-play" type="button" onclick={rdPlayScreen}>
                <span class="rd-viewer__big-play-icon">▶</span>
                <span>Reprendre la diffusion</span>
              </button>
              <p>Émission suspendue. Clique pour la reprendre.</p>
            </div>
          {/if}

          <!-- Barre flottante d'actions (transparente, fade-in au survol) -->
          <div
            class="rd-viewer__floating-actions"
            class:visible={viewerControlsVisible || !viewerRemoteStream || !rdScreenPlayRequested}>
            <button
              class="rd-viewer__fab"
              type="button"
              onclick={() => { selectedFeature = null; }}
              title="Retour au menu">
              <span class="rd-viewer__fab-icon">←</span>
              <span class="rd-viewer__fab-label">Menu</span>
            </button>
            {#if rdScreenPlayRequested}
              <button
                class="rd-viewer__fab"
                type="button"
                onclick={rdPauseScreen}
                disabled={rdVideoPausedForTransfer}
                title="Suspendre l'émission des frames">
                <span class="rd-viewer__fab-icon">⏸</span>
                <span class="rd-viewer__fab-label">Pause</span>
              </button>
            {:else}
              <button
                class="rd-viewer__fab rd-viewer__fab--accent"
                type="button"
                onclick={rdPlayScreen}
                disabled={rdVideoPausedForTransfer}
                title="Démarrer l'émission de frames">
                <span class="rd-viewer__fab-icon">▶</span>
                <span class="rd-viewer__fab-label">Play</span>
              </button>
            {/if}
            <button
              class="rd-viewer__fab"
              type="button"
              onclick={rdTriggerFilePicker}
              disabled={!rdFileChannelLive}
              title={rdFileChannelLive ? "Envoyer un fichier" : "Canal fichier non disponible"}>
              <span class="rd-viewer__fab-icon">📤</span>
              <span class="rd-viewer__fab-label">Fichier</span>
            </button>
            <button
              class="rd-viewer__fab"
              type="button"
              onclick={() => void enterViewerFullscreen()}
              disabled={!viewerRemoteStream || viewerFullscreenActive}
              title="Plein écran">
              <span class="rd-viewer__fab-icon">⛶</span>
              <span class="rd-viewer__fab-label">Plein écran</span>
            </button>
            <button
              class="rd-viewer__fab"
              type="button"
              onclick={() => void exitViewerFullscreen()}
              disabled={!viewerFullscreenActive}
              title="Quitter le plein écran">
              <span class="rd-viewer__fab-icon">⤢</span>
              <span class="rd-viewer__fab-label">Quitter</span>
            </button>
            <button
              class="rd-viewer__fab rd-viewer__fab--danger"
              type="button"
              onclick={() => void stopByToken()}
              disabled={actionLoading}
              title="Déconnecter la session">
              <span class="rd-viewer__fab-icon">⏻</span>
              <span class="rd-viewer__fab-label">Déconnecter</span>
            </button>
          </div>
        </div>

        <!-- Input fichier caché (déclenché par le bouton flottant) -->
        <input
          bind:this={rdFileInputEl}
          type="file"
          multiple
          style="display:none"
          onchange={rdHandleFilePicked} />

        <!-- Panneau de transferts : visible dès qu'il y a au moins un transfer -->
        {#if Object.keys(fileTransfers).length > 0}
          <div class="rd-transfers">
            <div class="rd-transfers__head">
              <strong>Transferts de fichiers</strong>
              <span class="rd-transfers__hint">
                {fileChannelOpen ? "Canal P2P ouvert" : "Canal en attente…"}
              </span>
            </div>
            {#each Object.values(fileTransfers).sort((a, b) => b.startedAt - a.startedAt) as t (t.transferId)}
              <article class="rd-transfer rd-transfer--{t.state}">
                <div class="rd-transfer__icon">
                  {t.type === "upload" ? "📤" : "📥"}
                </div>
                <div class="rd-transfer__body">
                  <div class="rd-transfer__line">
                    <strong class="rd-transfer__name">{t.fileName}</strong>
                    <span class="rd-transfer__meta">
                      {rdFormatBytes(t.doneBytes)} / {rdFormatBytes(t.totalSize)}
                      {#if t.state === "active"}&nbsp;•&nbsp; {rdProgressPercent(t)}%{/if}
                    </span>
                  </div>
                  <div class="rd-transfer__bar">
                    <div class="rd-transfer__bar-fill" style="width: {rdProgressPercent(t)}%"></div>
                  </div>
                  {#if t.state === "error"}
                    <p class="rd-transfer__error">Erreur : {t.error ?? "inconnue"}</p>
                  {:else if t.state === "complete"}
                    <p class="rd-transfer__done">
                      {t.type === "upload" ? "Envoyé à l'autre PC" : "Reçu et téléchargé"}
                    </p>
                  {/if}
                </div>
                {#if t.state !== "active"}
                  <button
                    class="rd-transfer__close"
                    type="button"
                    onclick={() => rdDismissTransfer(t.transferId)}
                    title="Retirer de la liste">×</button>
                {/if}
              </article>
            {/each}
          </div>
        {/if}

        {#if screenFrameError}
          <p class="rd-connect__status rd-connect__status--error">{screenFrameError}</p>
        {/if}
      </section>
    {/if}

    <section class="rd-panel">
      <h2 class="rd-panel__title"><span class="rd-icon">🖥</span> Métriques de cette machine</h2>
      <div class="rd-metrics">
        <div class="rd-metric">
          <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--cpu">⚙</span> CPU</div>
          <div class="rd-metric__value">{metrics ? `${metrics.cpuUsage.toFixed(0)}%` : "24%"}</div>
        </div>
        <div class="rd-metric">
          <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--ram">🗄</span> RAM</div>
          <div class="rd-metric__value">{metrics ? `${(metrics.ramUsage / 100 * 16).toFixed(1)} / 16 GB` : "8.2 / 16 GB"}</div>
        </div>
        <div class="rd-metric">
          <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--disk">💾</span> Disque</div>
          <div class="rd-metric__value">{metrics ? `${(metrics.diskUsage / 100 * 512).toFixed(0)} / 512 GB` : "256 / 512 GB"}</div>
        </div>
        <div class="rd-metric">
          <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--net">📶</span> Réseau</div>
          <div class="rd-metric__value rd-metric__value--ok">Connected</div>
        </div>
      </div>
    </section>

    <div class="rd-history-grid">
      <section class="rd-panel rd-history">
        <header class="rd-history__head">
          <h2 class="rd-panel__title"><span class="rd-icon">⏱</span> Historique des sessions</h2>
          <span class="rd-history__count">{rdFilteredSessions.length} session{rdFilteredSessions.length > 1 ? "s" : ""}</span>
        </header>
        <input
          class="rd-history__search"
          type="search"
          placeholder="Rechercher par code machine..."
          bind:value={rdSessionSearch} />
        <div class="rd-history__filters">
          <select class="rd-select" bind:value={rdSessionTypeFilter}>
            <option value="all">Tous les types</option>
            <option value="incoming">Entrantes</option>
            <option value="outgoing">Sortantes</option>
          </select>
          <select class="rd-select" bind:value={rdSessionStatusFilter}>
            <option value="all">Tous les statuts</option>
            <option value="active">En cours</option>
            <option value="ended">Terminées</option>
          </select>
        </div>
        <div class="rd-history__list">
          {#if rdSessionError}
            <p class="rd-empty">Erreur API: {rdSessionError}</p>
          {:else if rdSessionLoading && rdFilteredSessions.length === 0}
            <p class="rd-empty">Chargement…</p>
          {:else if rdFilteredSessions.length === 0}
            <p class="rd-empty">Aucune session pour les filtres actuels.</p>
          {:else}
            {#each rdFilteredSessions as session (session.id)}
              {@const isActive = session.status !== "TERMINATED"}
              <article class="rd-session">
                <div class="rd-session__top">
                  <strong class="rd-session__code">{session.peerLabel}</strong>
                  {#if isActive}
                    <span class="rd-pill rd-pill--live">En cours</span>
                  {:else}
                    <span class="rd-pill rd-pill--done">Terminée</span>
                  {/if}
                </div>
                <p class="rd-session__type">
                  Connexion {session.direction === "incoming" ? "entrante" : "sortante"}
                </p>
                <p class="rd-session__meta">
                  Début: {rdFormatTime(session.startedAt)}
                  {#if !isActive && session.durationMs}
                    &nbsp;&nbsp; Durée: {rdFormatDuration(session.durationMs)}
                  {/if}
                </p>
              </article>
            {/each}
          {/if}
        </div>
      </section>

      <section class="rd-panel rd-history">
        <header class="rd-history__head">
          <h2 class="rd-panel__title"><span class="rd-icon">📄</span> Historique des fichiers</h2>
          <span class="rd-history__count">{rdFilteredFiles.length} fichier{rdFilteredFiles.length > 1 ? "s" : ""}</span>
        </header>
        <input
          class="rd-history__search"
          type="search"
          placeholder="Rechercher par nom de fichier ou code machine..."
          bind:value={rdFileSearch} />
        <div class="rd-history__filters">
          <select class="rd-select" bind:value={rdFileFilter}>
            <option value="all">Tous les transferts</option>
            <option value="upload">Fichiers envoyés</option>
            <option value="download">Fichiers reçus</option>
          </select>
        </div>
        <div class="rd-history__list">
          {#if rdFilteredFiles.length === 0}
            <p class="rd-empty">Aucun transfert pour les filtres actuels.</p>
          {:else}
            {#each rdFilteredFiles as file (file.transferId)}
              <article class="rd-file">
                <span class="rd-file__icon {rdFileIconClass(file.fileName)}">📄</span>
                <div class="rd-file__body">
                  <strong class="rd-file__name">{file.fileName}</strong>
                  <p class="rd-file__sub">
                    {file.type === "upload" ? "Envoyé" : "Reçu"}
                    {#if activeSession?.agentMachineId}
                      {file.type === "upload" ? "vers" : "de"} {activeSession.agentMachineId}
                    {/if}
                  </p>
                  <p class="rd-file__meta">
                    {rdFormatBytes(file.totalSize || file.doneBytes)} &nbsp;•&nbsp; {rdFormatRelative(file.startedAt)}
                    {#if file.state !== "complete"}
                      &nbsp;•&nbsp; <span class="rd-file__state">{file.state}</span>
                    {/if}
                  </p>
                </div>
              </article>
            {/each}
          {/if}
        </div>
      </section>
    </div>
  </section>

  <!-- ── Modal d'approbation : popup côté ordinateur DISTANT (cible) ── -->
  {#if showApprovalModal && pendingApprovalSession}
    <div
      class="rd-approval-overlay"
      role="dialog"
      tabindex="-1"
      onkeydown={(e) => { if (e.key === "Escape" && !approvalLoading) showApprovalModal = false; }}
      onmousedown={(e) => { if (!approvalLoading && e.target === e.currentTarget) showApprovalModal = false; }}>
      <div class="rd-approval-modal">
        <h2>Demande d'accès distant</h2>
        <p class="rd-approval-desc">
          <strong>{pendingApprovalSession.technicianUsername || "Un technicien"}</strong>
          demande l'accès à ce PC.
        </p>

        {#if approvalError}
          <p class="rd-approval-error">{approvalError}</p>
        {/if}

        <div class="rd-approval-options">
          <label>
            <input type="checkbox" bind:checked={approvalAllowRemoteInput} disabled={approvalLoading} />
            Autoriser clavier / souris
          </label>
          <label>
            <input type="checkbox" bind:checked={approvalAllowFileTransfer} disabled={approvalLoading} />
            Autoriser transfert de fichiers
          </label>
        </div>

        <div class="rd-approval-actions">
          <button
            class="rd-approval-btn rd-approval-btn--reject"
            onclick={rejectPendingSession}
            disabled={approvalLoading}>
            {approvalLoading ? "Traitement…" : "Refuser"}
          </button>
          <button
            class="rd-approval-btn rd-approval-btn--approve"
            onclick={approvePendingSession}
            disabled={approvalLoading}>
            {approvalLoading ? "Traitement…" : "Autoriser"}
          </button>
        </div>
      </div>
    </div>
  {/if}

  <!-- ╔══════════════════════════════════════════════════════════════╗
       ║  ANCIEN UI — bloc commenté (HTML comment, ignoré par Svelte).║
       ║  Aucun backend Rust touché.                                   ║
       ╚══════════════════════════════════════════════════════════════╝
  <header class="hero">
    <div class="hero-copy">
      <p class="eyebrow">Support distant</p>
      <h1>Lumiere IT</h1>
      <p class="hero-text">
        Console technicien pour lancer une session, voir l'etat de la machine cible et prendre la main a distance.
      </p>
    </div>
    <div class="badges status-strip">
      <span class="badge" class:ok={!metricsError && !metricsLoading} class:error={!!metricsError}>
        Mesures locales: {metricsLoading ? "chargement" : metricsError ? "erreur" : "ok"}
      </span>
      <span class="badge" class:ok={agentRunning} class:error={!agentRunning}>
        Agent local: {agentRunning ? "actif" : "arret"}
      </span>
      <span class="badge" class:ok={!agentsError && !agentsLoading} class:error={!!agentsError}>
        API: {agentsLoading ? "chargement" : agentsError ? "erreur" : "ok"}
      </span>
      <span class="badge" class:ok={signalingConnected} class:error={!signalingConnected}>
        Signal: {signalingConnected ? "connecte" : "hors ligne"}
      </span>
      <span class="badge" class:ok={chatConnected} class:error={!chatConnected}>
        Chat: {chatConnected ? "connecte" : "attente"}
      </span>
    </div>
  </header>

  <section class="card metrics-panel">
    <button class="metrics-summary" type="button" onclick={toggleMetricsPanel}>
      <div>
        <h2>Mesures de cet agent</h2>
        <p class="hint top-gap">
          {metricsPanelOpen ? "Cliquez pour masquer le detail." : "Cliquez pour afficher CPU, RAM et disque."}
        </p>
      </div>
      <div class="row">
        <span class={`pill ${metrics && !metricsError ? "ok" : "muted"}`}>
          {metricsLoading ? "chargement" : metricsError ? "indisponible" : "mesures recues"}
        </span>
        <span class="pill muted">{metricsPanelOpen ? "masquer" : "ouvrir"}</span>
      </div>
    </button>

    {#if metricsPanelOpen}
      <div class="grid metrics top-gap">
        <article class="metric-card metric-tile">
          <h3>CPU</h3>
          <p class="big">{metrics ? `${metrics.cpuUsage.toFixed(1)}%` : "-"}</p>
        </article>
        <article class="metric-card metric-tile">
          <h3>RAM</h3>
          <p class="big">{metrics ? `${metrics.ramUsage.toFixed(1)}%` : "-"}</p>
        </article>
        <article class="metric-card metric-tile">
          <h3>Disque</h3>
          <p class="big">{metrics ? `${metrics.diskUsage.toFixed(1)}%` : "-"}</p>
        </article>
      </div>
      <p class="hint top-gap">
        {metrics
          ? `Derniere mesure: ${new Date(metrics.timestamp).toLocaleTimeString()}`
          : metricsError || "Aucune mesure disponible pour le moment."}
      </p>
    {/if}
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
          <div class="item agent-item">
            <div class="agent-meta">
              <strong>{agent.machineId}</strong>
              <p class="hint">{agent.hostname} - {agent.osInfo}</p>
            </div>
            <div class="row">
              <span class={`pill ${statusClass(agent.status)}`}>{agent.status}</span>
              <button onclick={() => startSession(agent.machineId)} disabled={actionLoading}>Se connecter</button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <section class="grid actions">
    <article class="card">
      <h2>Code de connexion de cet agent</h2>
      <p class="hint top-gap">Partagez ce code au technicien pour un demarrage rapide.</p>
      <div class="connection-code-card top-gap">
        <div>
          <span class="session-kv-label">Machine locale</span>
          <strong>{localMachineId || "indisponible"}</strong>
        </div>
        <div class="connection-code-row">
          <span class="connection-code-value">
            {localConnectionCodeLoading ? "Chargement..." : localConnectionCode || "Code indisponible"}
          </span>
          <button onclick={copyLocalConnectionCode} disabled={!localConnectionCode}>Copier</button>
        </div>
        {#if connectionCodeCopied}
          <p class="hint ok">Code copie.</p>
        {/if}
        {#if localConnectionCodeError}
          <p class="error top-gap">{localConnectionCodeError}</p>
        {/if}
      </div>
    </article>

    <article class="card">
      <h2>Connexion par code</h2>
      <div class="row">
        <input bind:value={connectionCode} placeholder="Code de connexion" />
        <button onclick={startSessionWithCode} disabled={actionLoading}>Lancer</button>
      </div>
    </article>
  </section>

  <section class="card session-card">
    <div class="row between">
      <div>
        <h2>Session courante</h2>
        <p class="hint top-gap">Suivi de la machine cible et acces aux fonctions de support.</p>
      </div>
      {#if queriedSession}
        <div class="row">
          <span class={`pill ${statusClass(queriedSession.status)}`}>{queriedSession.status}</span>
          <span class={`pill ${shouldBridgeSessionToLocalAgent(queriedSession) ? "warn" : "ok"}`}>
            {shouldBridgeSessionToLocalAgent(queriedSession) ? "machine locale" : "machine distante"}
          </span>
        </div>
      {/if}
    </div>

    {#if queriedSession}
      <div class="session-grid top-gap">
        <div class="session-kv">
          <span class="session-kv-label">Machine cible</span>
          <strong>{queriedSession.agentMachineId}</strong>
        </div>
        <div class="session-kv">
          <span class="session-kv-label">Technicien</span>
          <strong>{queriedSession.technicianUsername || "viewer"}</strong>
        </div>
        <div class="session-kv">
          <span class="session-kv-label">Identifiant</span>
          <strong>#{queriedSession.id}</strong>
        </div>
        <div class="session-kv">
          <span class="session-kv-label">Token</span>
          <code>{queriedSession.signalingToken}</code>
        </div>
      </div>

      {#if waitingForApproval || queriedSession.status === "PENDING_APPROVAL"}
        <p class="hint top-gap waiting-msg">
          Demande envoyee. En attente de confirmation sur le PC distant...
        </p>
      {/if}

      {#if queriedSession.status === "ACTIVE"}
        <div class="row top-gap feature-actions session-toolbar">
          <button
            class:selected={selectedFeature === "screen"}
            onclick={() => chooseFeature("screen")}
          >
            Ecran distant
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
            Fichiers
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
  <section class:expanded={viewerExpanded} class="card remote-session-card">
    <div class="row between">
      <div class="remote-session-heading">
        <h2>Controle distant</h2>
        <p class="hint top-gap">Acces visuel temps reel avec commandes souris et clavier sur la machine distante.</p>
      </div>
      <div class="row remote-session-actions">
        <button onclick={toggleViewerExpanded}>
          {viewerExpanded ? "Taille normale" : "Agrandir"}
        </button>
        <button onclick={() => void toggleViewerFullscreen()} disabled={!viewerRemoteStream}>
          {viewerFullscreenActive ? "Quitter plein ecran" : "Plein ecran"}
        </button>
        <button onclick={() => void connectSignaling()} disabled={actionLoading || signalingConnected}>Reconnecter</button>
        <button class="danger-ghost" onclick={() => void disconnectSignaling({ sendLeave: true })} disabled={!signalingConnected}>Deconnecter</button>
      </div>
    </div>

    {#if signalingError}
      <p class="error top-gap">{signalingError}</p>
    {/if}

    {#if backendSyncError}
      <p class="error top-gap">{backendSyncError}</p>
    {/if}

    <div class="top-gap viewer-status-bar">
      <div class="viewer-status-summary">
        <div class="viewer-summary-tile">
          <span class="session-kv-label">Etat</span>
          <div class="viewer-status-stack">
            <span class={`pill ${viewerStateClass(viewerConnectionState)}`}>{viewerStateLabel(viewerConnectionState)}</span>
            <span class={`pill ${viewerRemoteStream ? "ok" : "muted"}`}>
              {viewerRemoteStream ? "flux live" : "en attente"}
            </span>
          </div>
        </div>
        <div class="viewer-summary-tile">
          <span class="session-kv-label">Controle</span>
          <span class={`pill ${queriedSession?.allowRemoteInput === false ? "warn" : viewerDataChannelOpen ? "ok" : "muted"}`}>
            {queriedSession?.allowRemoteInput === false
              ? "lecture seule"
              : viewerDataChannelOpen
                ? viewerKeyboardCaptured
                  ? "clavier + souris actifs"
                  : "souris active - cliquez la video pour le clavier"
                : "input en attente"}
          </span>
        </div>
        <div class="viewer-summary-tile">
          <span class="session-kv-label">Qualite</span>
          <div class="viewer-status-stack">
            <span class={`pill ${viewerQualityClass(viewerStreamMbps)}`}>{viewerQualityLabel(viewerStreamMbps)}</span>
            <span class="pill muted">preset: {viewerPreset}</span>
            {#if viewerStreamMbps !== null}
              <span class="pill ok">{viewerStreamMbps.toFixed(2)} Mbps</span>
            {/if}
            {#if viewerStreamFps !== null}
              <span class="pill muted">{viewerStreamFps.toFixed(1)} FPS</span>
            {/if}
          </div>
        </div>
        <div class="viewer-summary-tile">
          <span class="session-kv-label">Affichage</span>
          <div class="viewer-status-stack">
            {#if viewerRemoteStream}
              <span class="pill muted">{viewerRemoteWidth}x{viewerRemoteHeight}</span>
            {/if}
            <span class="pill muted">{viewerFullscreenActive ? "plein ecran" : viewerExpanded ? "agrandi" : "standard"}</span>
          </div>
        </div>
      </div>
      <p class="hint control-hint">
        Bougez la souris dans la video pour afficher les commandes. Cliquez dans la video pour activer le clavier distant.
      </p>
    </div>

    <div class="top-gap screen-frame-panel">
      <div bind:this={viewerShellEl} class="video-shell" role="presentation" onmousemove={revealViewerControls}>
        <div class:visible={viewerControlsVisible || viewerConnectionState !== "connected"} class="remote-toolbar">
          <div class="viewer-toolbar-group viewer-toolbar-status">
            <span class={`pill ${viewerStateClass(viewerConnectionState)}`}>{viewerStateLabel(viewerConnectionState)}</span>
            <span class={`pill ${queriedSession?.allowRemoteInput === false ? "warn" : viewerDataChannelOpen ? "ok" : "muted"}`}>
              {queriedSession?.allowRemoteInput === false
                ? "lecture seule"
                : viewerDataChannelOpen
                  ? "controle actif"
                  : "input en attente"}
            </span>
            <span class={`pill ${viewerQualityClass(viewerStreamMbps)}`}>{viewerQualityLabel(viewerStreamMbps)}</span>
            <span class="telemetry-pill">
              <span class="telemetry-label">LIVE</span>
              <span class={`telemetry-dot ${viewerStreamMbps !== null || viewerStreamFps !== null ? "ok" : "muted"}`}></span>
            </span>
            <span class="telemetry-pill">
              <span class="telemetry-label">FPS</span>
              <strong>{viewerStreamFps !== null ? viewerStreamFps.toFixed(1) : "--"}</strong>
            </span>
            <span class="telemetry-pill">
              <span class="telemetry-label">Mbps</span>
              <strong>{viewerStreamMbps !== null ? viewerStreamMbps.toFixed(2) : "--"}</strong>
            </span>
            {#if viewerRemoteStream}
              <span class="pill muted">{viewerRemoteWidth}x{viewerRemoteHeight}</span>
            {/if}
          </div>
          <div class="viewer-toolbar-group viewer-toolbar-actions">
            <select class="toolbar-btn" bind:value={viewerFpsTier} onchange={applyViewerStreamTuning}>
              <option value="auto">FPS auto</option>
              <option value="idle">FPS 15 (idle)</option>
              <option value="normal">FPS 30 (normal)</option>
              <option value="active">FPS 60 (active)</option>
            </select>
            <select class="toolbar-btn" bind:value={viewerBitrateTier} onchange={applyViewerStreamTuning}>
              <option value="auto">Bitrate auto</option>
              <option value="poor">1.5 Mbps</option>
              <option value="medium">4 Mbps</option>
              <option value="good">8 Mbps</option>
            </select>
            <button
              class="toolbar-btn"
              class:selected={viewerPreset === "low-latency"}
              onclick={() => applyViewerPreset("low-latency")}
            >
              Low latency
            </button>
            <button
              class="toolbar-btn"
              class:selected={viewerPreset === "balanced"}
              onclick={() => applyViewerPreset("balanced")}
            >
              Balanced
            </button>
            <button
              class="toolbar-btn"
              class:selected={viewerPreset === "quality"}
              onclick={() => applyViewerPreset("quality")}
            >
              Quality
            </button>
            <button class="toolbar-btn" onclick={toggleViewerPlaybackProfile}>
              {viewerPlaybackProfile === "quality" ? "Mode qualite" : "Mode reactif"}
            </button>
            <button class="toolbar-btn" onclick={toggleViewerExpanded}>
              {viewerExpanded ? "Normal" : "Agrandir"}
            </button>
            <button class="toolbar-btn" onclick={() => void toggleViewerFullscreen()} disabled={!viewerRemoteStream}>
              {viewerFullscreenActive ? "Quitter" : "Plein ecran"}
            </button>
            <button class="toolbar-btn danger-ghost" onclick={() => void disconnectSignaling({ sendLeave: true })} disabled={!signalingConnected}>
              Deconnecter
            </button>
          </div>
        </div>

        {#if viewerRemoteStream}
          <video
            class="viewer-video"
            class:active={canSendViewerInput()}
            bind:this={viewerVideoEl}
            autoplay
            playsinline
            muted
            tabindex="0"
            onfocus={handleViewerVideoFocus}
            onblur={handleViewerVideoBlur}
            onmousemove={handleViewerMouseMove}
            onmousedown={handleViewerMouseDown}
            onmouseup={handleViewerMouseUp}
            onwheel={handleViewerWheel}
            oncontextmenu={(event) => event.preventDefault()}
          ></video>
        {:else}
          <div class="video-placeholder">
            <p>Aucune image reÃ§ue pour le moment.</p>
            <p class="hint">Lance la session puis attends la premiere frame WebRTC de l'agent.</p>
          </div>
        {/if}
      </div>

      {#if screenFrameError}
        <p class="error top-gap">{screenFrameError}</p>
      {/if}
    </div>

    {#if uiDebugEnabled}
      <details class="debug-panel top-gap">
        <summary>Diagnostic signaling ({signalLogs.length})</summary>

        {#if signalLogs.length === 0}
          <p class="hint debug-empty">Aucun evenement signaling pour le moment.</p>
        {:else}
          {#each signalLogs as log, i (`${log.timestamp}-${i}`)}
            <div class="signal-log">
              <div class="signal-log-head">
                <p class="mono">
                  [{log.timestamp}] {log.direction.toUpperCase()} {log.type} {log.from} -&gt; {log.to}
                </p>
              </div>
              <p class="hint mono signal-log-payload">{log.payload || "(no payload)"}</p>
            </div>
          {/each}
        {/if}
      </details>
    {/if}
  </section>
  {/if}

  {#if queriedSession?.status === "ACTIVE" && selectedFeature === "chat"}
  <section class="card">
    <div class="row between">
      <div>
        <h2>Chat</h2>
        <p class="hint top-gap">Messagerie temps rÃ©el de la session.</p>
      </div>
      <div class="row">
        <span class={`pill ${chatConnected ? "ok" : "warn"}`}>
          {chatConnected ? "connectÃ©" : "hors ligne"}
        </span>
        {#if !chatConnected}
          <button onclick={() => void connectChat()}>Reconnecter</button>
        {/if}
      </div>
    </div>

    {#if chatError}
      <p class="error top-gap">{chatError}</p>
    {/if}

    {#if chatMessages.length === 0}
      <p class="hint top-gap">Aucun message pour l'instant.</p>
    {:else}
      <div class="list top-gap chat-list">
        {#each chatMessages as msg (msgKey(msg))}
          <div class="item chat-item" class:chat-self={msg.senderName === "viewer"}>
            <p class="chat-bubble"><strong>{msg.senderName}</strong>: {msg.content}</p>
            <p class="hint mono chat-ts">{new Date(msg.timestamp).toLocaleTimeString()}</p>
          </div>
        {/each}
      </div>
    {/if}

    {#if typingInfo}
      <p class="hint top-gap chat-typing">{typingInfo.senderName} est en train d'Ã©crireâ€¦</p>
    {/if}

    <div class="row top-gap">
      <input
        bind:value={chatInput}
        placeholder="Votre messageâ€¦"
        class="chat-input"
        onkeydown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void sendChatMessage(); } }}
        oninput={() => {
          const roomId = chatRoomId || resolveRoomId();
          if (roomId) chatClient.sendTyping(roomId, "viewer", "viewer", true);
        }}
      />
      <button onclick={() => void sendChatMessage()} disabled={!chatInput.trim()}>Envoyer</button>
    </div>
  </section>
  {/if}

  {#if queriedSession?.status === "ACTIVE" && selectedFeature === "files"}
    <section class="card">
      <div class="row between">
        <div>
          <h2>Transfert de fichiers</h2>
          <p class="hint top-gap">Navigation et transfert P2P via DataChannel WebRTC.</p>
        </div>
        <span class={`pill ${fileChannelOpen ? "ok" : "warn"}`}>
          {fileChannelOpen ? "canal pret" : "canal en attente"}
        </span>
      </div>

      {#if queriedSession.allowFileTransfer === false}
        <p class="error top-gap">Transfert de fichiers non autorise pour cette session.</p>
      {:else}
        <div class="top-gap">
          <h3>Envoyer un fichier vers l'agent</h3>
          <div class="row top-gap">
            <input
              type="file"
              disabled={!fileChannelOpen}
              onchange={async (e) => {
                const input = e.currentTarget as HTMLInputElement;
                const file = input.files?.[0];
                if (file) {
                  input.value = "";
                  await uploadLocalFile(file);
                }
              }}
              class="file-input"
            />
          </div>
        </div>

        <div class="top-gap">
          <div class="row between">
            <h3>Explorateur distant</h3>
            <div class="row">
              <button
                disabled={!fileChannelOpen || fileListLoading}
                onclick={() => requestFileList(fileCurrentPath)}
              >
                {fileListLoading ? "Chargement..." : "Actualiser"}
              </button>
              {#if fileCurrentPath}
                <button
                  disabled={!fileChannelOpen}
                  onclick={() => {
                    const parent = fileCurrentPath.replace(/[/\\][^/\\]*$/, "") || "";
                    requestFileList(parent);
                  }}
                >
                  Dossier parent
                </button>
              {/if}
            </div>
          </div>

          {#if !fileChannelOpen}
            <p class="hint top-gap">En attente du canal fichier WebRTC...</p>
          {:else if fileListing.length === 0 && !fileListLoading && !fileListError && !fileCurrentPath}
            <div class="top-gap">
              <button onclick={() => requestFileList("")}>
                Parcourir les fichiers distants
              </button>
            </div>
          {:else}
            {#if fileListError}
              <p class="error top-gap">{fileListError}</p>
            {/if}

            {#if fileCurrentPath}
              <p class="hint top-gap mono">{fileCurrentPath}</p>
            {/if}

            <div class="list top-gap">
              {#each fileListing as entry (entry.path)}
                <div class="item file-item">
                  <div class="file-meta">
                    <span class="file-icon">{entry.isDirectory ? "ðŸ“" : "ðŸ“„"}</span>
                    <span class="file-name">{entry.name}</span>
                    {#if !entry.isDirectory && entry.size > 0}
                      <span class="hint">{formatFileSize(entry.size)}</span>
                    {/if}
                  </div>
                  <div class="row">
                    {#if entry.isDirectory}
                      <button
                        class="btn-sm"
                        onclick={() => requestFileList(entry.path)}
                      >
                        Ouvrir
                      </button>
                    {:else}
                      <button
                        class="btn-sm"
                        disabled={!fileChannelOpen}
                        onclick={() => downloadRemoteFile(entry.path, entry.name)}
                      >
                        TÃ©lÃ©charger
                      </button>
                    {/if}
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        {#if Object.keys(fileTransfers).length > 0}
          <div class="top-gap">
            <div class="row between">
              <h3>Transferts</h3>
              <button
                class="btn-sm"
                onclick={() => {
                  const cleaned: Record<string, FileTransfer> = {};
                  for (const [k, v] of Object.entries(fileTransfers)) {
                    if (v.state === "active") cleaned[k] = v;
                  }
                  fileTransfers = cleaned;
                }}
              >
                Effacer terminÃ©s
              </button>
            </div>
            <div class="list top-gap">
              {#each Object.values(fileTransfers).slice().reverse() as t (t.transferId)}
                <div class="item transfer-item">
                  <div class="transfer-meta">
                    <span class="transfer-direction">
                      {t.type === "upload" ? "â¬† Upload" : "â¬‡ Download"}
                    </span>
                    <span class="file-name">{t.fileName}</span>
                    <span class={`pill ${t.state === "complete" ? "ok" : t.state === "error" ? "error" : "warn"}`}>
                      {t.state === "active"
                        ? `${transferProgress(t)}%`
                        : t.state === "complete"
                          ? "terminÃ©"
                          : "erreur"}
                    </span>
                  </div>
                  {#if t.state === "active"}
                    <div class="progress-bar-wrap">
                      <div
                        class="progress-bar-fill"
                        style="width: {transferProgress(t)}%"
                      ></div>
                    </div>
                    <p class="hint">
                      {formatFileSize(t.doneBytes)} / {formatFileSize(t.totalSize)}
                      {#if transferSpeed(t)}Â· {transferSpeed(t)}{/if}
                    </p>
                  {:else if t.state === "error"}
                    <p class="error">{t.error ?? "Erreur inconnue"}</p>
                  {:else}
                    <p class="hint">{formatFileSize(t.totalSize)} â€” terminÃ©</p>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
        {/if}
      {/if}
    </section>
  {/if}

  {#if showApprovalModal && pendingApprovalSession}
    <div
      class="approval-overlay"
      role="dialog"
      tabindex="-1"
      onkeydown={(e) => e.key === "Escape" && !approvalLoading && (showApprovalModal = false)}
      onmousedown={(e) => !approvalLoading && e.target === e.currentTarget && (showApprovalModal = false)}
    >
      <div class="approval-modal">
        <h2>Demande d'accÃ¨s distant</h2>
        <p class="approval-desc">
          <strong>{pendingApprovalSession.technicianUsername || "Technicien"}</strong> demande l'accÃ¨s Ã  ce PC.
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
  /fin du bloc ancien UI commenté -->
</main>

<style>
  /* ═══════════════════════════════════════════════════════════════
     Styles "Bureau à Distance" (nouvelle UI — voir maquette).
     Tout est préfixé .rd-* pour ne pas entrer en conflit avec les
     styles legacy plus bas.
     ═══════════════════════════════════════════════════════════════ */
  .rd-page {
    min-height: 100vh;
    padding: 24px 32px;
    background: #0d1117;
    color: #e2e8f0;
    font-family: "Segoe UI", system-ui, sans-serif;
    box-sizing: border-box;
  }

  .rd-card {
    width: 100%;
    max-width: 1800px;
    margin: 0 auto;
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  /* Chaque sous-bloc devient sa propre carte pleine largeur (cf. maquette) */
  .rd-card > .rd-panel,
  .rd-card > .rd-history-grid > .rd-panel {
    background: #11181f;
    border: 1px solid #1f2a36;
    border-radius: 16px;
    padding: 22px 24px;
  }
  .rd-header {
    background: transparent;
    border: none;
    padding: 0 4px;
  }

  .rd-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 24px;
    flex-wrap: wrap;
  }
  .rd-title h1 {
    margin: 0 0 6px 0;
    font-size: 28px;
    font-weight: 700;
    color: #fff;
  }
  .rd-title p {
    margin: 0;
    color: #94a3b8;
    font-size: 14px;
  }
  .rd-machine-code {
    background: #0f1620;
    border: 1px solid #1f2a36;
    border-radius: 12px;
    padding: 12px 18px;
    text-align: right;
    min-width: 220px;
  }
  .rd-machine-code__label {
    display: block;
    font-size: 11px;
    color: #94a3b8;
    margin-bottom: 4px;
  }
  .rd-machine-code__value {
    display: block;
    font-family: "Consolas", "SF Mono", monospace;
    font-weight: 700;
    font-size: 18px;
    color: #38bdf8;
    letter-spacing: 0.5px;
  }

  .rd-panel {
    background: #0f1620;
    border: 1px solid #1f2a36;
    border-radius: 12px;
    padding: 18px 20px;
  }
  .rd-panel__title {
    margin: 0 0 14px 0;
    font-size: 15px;
    font-weight: 600;
    color: #e2e8f0;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .rd-icon {
    color: #38bdf8;
  }

  /* ── Connexion par code ───────────────────────────────────────── */
  .rd-connect {
    display: flex;
    gap: 12px;
  }
  .rd-connect__input {
    flex: 1;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 8px;
    padding: 12px 14px;
    color: #e2e8f0;
    font-size: 14px;
  }
  .rd-connect__input::placeholder {
    color: #475569;
  }
  .rd-connect__input:focus {
    outline: none;
    border-color: #38bdf8;
  }
  .rd-connect__btn {
    background: #1f2a36;
    border: 1px solid #2a3a4a;
    color: #cbd5e1;
    padding: 12px 22px;
    border-radius: 8px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.15s;
  }
  .rd-connect__btn:hover {
    background: #2a3a4a;
  }

  /* ── Métriques ────────────────────────────────────────────────── */
  .rd-metrics {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 12px;
  }
  @media (max-width: 720px) {
    .rd-metrics { grid-template-columns: repeat(2, 1fr); }
  }
  .rd-metric {
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 10px;
    padding: 14px 16px;
  }
  .rd-metric__head {
    display: flex;
    align-items: center;
    gap: 8px;
    color: #94a3b8;
    font-size: 13px;
    margin-bottom: 8px;
  }
  .rd-metric__icon {
    width: 22px;
    height: 22px;
    border-radius: 6px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 12px;
  }
  .rd-metric__icon--cpu  { background: rgba(56,189,248,0.15); color: #38bdf8; }
  .rd-metric__icon--ram  { background: rgba(167,139,250,0.15); color: #a78bfa; }
  .rd-metric__icon--disk { background: rgba(192,132,252,0.15); color: #c084fc; }
  .rd-metric__icon--net  { background: rgba(74,222,128,0.15); color: #4ade80; }
  .rd-metric__value {
    font-size: 22px;
    font-weight: 700;
    color: #fff;
  }
  .rd-metric__value--ok { color: #4ade80; }

  /* ── Historique grille ────────────────────────────────────────── */
  .rd-history-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 18px;
  }
  @media (max-width: 900px) {
    .rd-history-grid { grid-template-columns: 1fr; }
  }
  .rd-history__head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
  }
  .rd-history__count {
    font-size: 12px;
    color: #94a3b8;
  }
  .rd-history__search {
    width: 100%;
    box-sizing: border-box;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 8px;
    padding: 10px 12px;
    color: #e2e8f0;
    font-size: 13px;
    margin-bottom: 10px;
  }
  .rd-history__search::placeholder { color: #475569; }
  .rd-history__filters {
    display: flex;
    gap: 10px;
    margin-bottom: 12px;
  }
  .rd-select {
    flex: 1;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 8px;
    padding: 8px 10px;
    color: #cbd5e1;
    font-size: 13px;
  }
  .rd-history__list {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: 320px;
    overflow-y: auto;
    padding-right: 4px;
  }

  /* ── Carte session ────────────────────────────────────────────── */
  .rd-session {
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 10px;
    padding: 12px 14px;
  }
  .rd-session__top {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 6px;
  }
  .rd-session__code {
    color: #38bdf8;
    font-family: "Consolas", monospace;
    font-size: 14px;
  }
  .rd-session__type {
    margin: 0 0 4px 0;
    font-size: 13px;
    color: #cbd5e1;
  }
  .rd-session__meta {
    margin: 0;
    font-size: 12px;
    color: #64748b;
  }
  .rd-pill {
    font-size: 11px;
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid transparent;
  }
  .rd-pill--done {
    background: rgba(148,163,184,0.12);
    color: #cbd5e1;
    border-color: rgba(148,163,184,0.2);
  }
  .rd-pill--live {
    background: rgba(74,222,128,0.15);
    color: #4ade80;
    border-color: rgba(74,222,128,0.3);
  }

  /* ── Carte fichier ────────────────────────────────────────────── */
  .rd-file {
    display: flex;
    gap: 12px;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 10px;
    padding: 12px 14px;
  }
  .rd-file__icon {
    width: 36px;
    height: 36px;
    border-radius: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
    flex-shrink: 0;
  }
  .rd-file__icon--pdf { background: rgba(56,189,248,0.15); color: #38bdf8; }
  .rd-file__icon--ppt { background: rgba(74,222,128,0.15); color: #4ade80; }
  .rd-file__icon--zip { background: rgba(56,189,248,0.15); color: #38bdf8; }
  .rd-file__body { flex: 1; min-width: 0; }
  .rd-file__name {
    display: block;
    color: #e2e8f0;
    font-size: 14px;
    margin-bottom: 2px;
  }
  .rd-file__sub {
    margin: 0 0 2px 0;
    font-size: 12px;
    color: #94a3b8;
  }
  .rd-file__meta {
    margin: 0;
    font-size: 11px;
    color: #64748b;
  }
  .rd-file__state {
    color: #38bdf8;
    text-transform: uppercase;
    font-weight: 600;
    letter-spacing: 0.5px;
  }
  .rd-empty {
    margin: 0;
    padding: 18px 8px;
    color: #64748b;
    font-size: 13px;
    text-align: center;
  }

  /* ── Bandeau de statut sous Connexion par code ─────────────────── */
  .rd-connect__status {
    margin-top: 14px;
    padding: 12px 16px;
    border-radius: 10px;
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 13px;
  }
  .rd-connect__status--waiting {
    background: rgba(56, 189, 248, 0.08);
    border: 1px solid rgba(56, 189, 248, 0.25);
    color: #cbd5e1;
  }
  .rd-connect__status--waiting strong { color: #38bdf8; }
  .rd-connect__status--waiting p { margin: 4px 0 0 0; color: #94a3b8; font-size: 12px; }
  .rd-connect__status--error {
    background: rgba(239, 68, 68, 0.08);
    border: 1px solid rgba(239, 68, 68, 0.3);
    color: #fca5a5;
  }
  .rd-connect__btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .rd-spinner {
    width: 18px;
    height: 18px;
    border: 2px solid rgba(56, 189, 248, 0.25);
    border-top-color: #38bdf8;
    border-radius: 50%;
    animation: rd-spin 0.8s linear infinite;
    flex-shrink: 0;
  }
  @keyframes rd-spin { to { transform: rotate(360deg); } }

  /* ── Modal d'approbation (popup côté machine cible) ────────────── */
  .rd-approval-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.65);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    backdrop-filter: blur(2px);
  }
  .rd-approval-modal {
    background: #11181f;
    border: 1px solid #1f2a36;
    border-radius: 14px;
    padding: 26px 28px;
    width: min(92vw, 460px);
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.55);
  }
  .rd-approval-modal h2 {
    margin: 0 0 8px 0;
    font-size: 18px;
    color: #fff;
  }
  .rd-approval-desc {
    margin: 0 0 18px 0;
    font-size: 14px;
    color: #cbd5e1;
  }
  .rd-approval-desc strong { color: #38bdf8; }
  .rd-approval-error {
    margin: 0 0 14px 0;
    padding: 8px 12px;
    border-radius: 8px;
    background: rgba(239, 68, 68, 0.12);
    border: 1px solid rgba(239, 68, 68, 0.3);
    color: #fca5a5;
    font-size: 13px;
  }
  .rd-approval-options {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-bottom: 22px;
  }
  .rd-approval-options label {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 8px;
    cursor: pointer;
    font-size: 14px;
    color: #e2e8f0;
  }
  .rd-approval-options label:hover { background: #0f1620; }
  .rd-approval-options input[type="checkbox"] {
    width: 16px;
    height: 16px;
    accent-color: #38bdf8;
  }
  .rd-approval-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
  }
  .rd-approval-btn {
    padding: 10px 20px;
    border-radius: 8px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    border: 1px solid transparent;
    transition: background 0.15s;
  }
  .rd-approval-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .rd-approval-btn--reject {
    background: transparent;
    border-color: #1f2a36;
    color: #cbd5e1;
  }
  .rd-approval-btn--reject:hover:not(:disabled) {
    background: rgba(239, 68, 68, 0.1);
    border-color: rgba(239, 68, 68, 0.4);
    color: #fca5a5;
  }
  .rd-approval-btn--approve {
    background: #38bdf8;
    color: #0d1117;
  }
  .rd-approval-btn--approve:hover:not(:disabled) { background: #7dd3fc; }

  /* ── Viewer vidéo (panneau visible pendant une session ACTIVE) ──── */
  .rd-viewer__head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 14px;
  }
  .rd-viewer__peer {
    color: #38bdf8;
    font-family: "Consolas", monospace;
    margin-left: 4px;
  }
  .rd-viewer__sub {
    margin: 4px 0 0 0;
    font-size: 12px;
    color: #94a3b8;
  }
  /* ── Barre flottante d'actions sur la vidéo ─────────────────────── */
  .rd-viewer__floating-actions {
    position: absolute;
    left: 50%;
    bottom: 18px;
    transform: translateX(-50%) translateY(8px);
    display: flex;
    gap: 8px;
    padding: 8px;
    background: rgba(15, 22, 32, 0.65);
    border: 1px solid rgba(56, 189, 248, 0.18);
    border-radius: 999px;
    backdrop-filter: blur(10px) saturate(1.2);
    -webkit-backdrop-filter: blur(10px) saturate(1.2);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.25s ease, transform 0.25s ease;
    z-index: 10;
  }
  .rd-viewer__floating-actions.visible {
    opacity: 1;
    pointer-events: auto;
    transform: translateX(-50%) translateY(0);
  }
  .rd-viewer__fab {
    background: transparent;
    border: 1px solid rgba(255, 255, 255, 0.08);
    color: #e2e8f0;
    padding: 8px 14px;
    border-radius: 999px;
    font-size: 13px;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s, color 0.15s;
    white-space: nowrap;
  }
  .rd-viewer__fab:hover:not(:disabled) {
    background: rgba(56, 189, 248, 0.12);
    border-color: rgba(56, 189, 248, 0.4);
    color: #fff;
  }
  .rd-viewer__fab:disabled { opacity: 0.35; cursor: not-allowed; }
  .rd-viewer__fab-icon { font-size: 15px; line-height: 1; }
  .rd-viewer__fab--danger { color: #fca5a5; }
  .rd-viewer__fab--danger:hover:not(:disabled) {
    background: rgba(239, 68, 68, 0.15);
    border-color: rgba(239, 68, 68, 0.45);
    color: #fff;
  }

  /* En plein écran, la barre se positionne par rapport au viewport */
  .rd-viewer__stage:fullscreen .rd-viewer__floating-actions,
  .rd-viewer__stage:-webkit-full-screen .rd-viewer__floating-actions {
    bottom: 28px;
  }

  /* Overlay "vidéo en pause pour transfert" */
  .rd-viewer__transfer-overlay {
    position: absolute;
    inset: 0;
    background: rgba(13, 17, 23, 0.85);
    backdrop-filter: blur(4px);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 16px;
    color: #cbd5e1;
    font-size: 14px;
    text-align: center;
    padding: 24px;
    z-index: 5;
  }
  .rd-viewer__transfer-overlay p { margin: 0; max-width: 420px; line-height: 1.5; }

  /* Panneau de transferts (sous la vidéo) */
  .rd-transfers {
    margin-top: 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .rd-transfers__head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    color: #cbd5e1;
    font-size: 13px;
  }
  .rd-transfers__hint { color: #64748b; font-size: 12px; }
  .rd-transfer {
    display: flex;
    gap: 12px;
    align-items: center;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 10px;
    padding: 10px 14px;
    position: relative;
  }
  .rd-transfer--complete { border-color: rgba(74, 222, 128, 0.35); }
  .rd-transfer--error { border-color: rgba(239, 68, 68, 0.4); }
  .rd-transfer__icon {
    width: 32px; height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 16px;
    border-radius: 8px;
    background: rgba(56, 189, 248, 0.12);
    flex-shrink: 0;
  }
  .rd-transfer__body { flex: 1; min-width: 0; }
  .rd-transfer__line {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 6px;
    font-size: 13px;
  }
  .rd-transfer__name { color: #e2e8f0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .rd-transfer__meta { color: #94a3b8; font-size: 12px; flex-shrink: 0; }
  .rd-transfer__bar {
    height: 4px;
    background: rgba(56, 189, 248, 0.12);
    border-radius: 2px;
    overflow: hidden;
  }
  .rd-transfer__bar-fill {
    height: 100%;
    background: linear-gradient(90deg, #38bdf8, #4ade80);
    transition: width 0.2s ease;
  }
  .rd-transfer--complete .rd-transfer__bar-fill { background: #4ade80; }
  .rd-transfer--error .rd-transfer__bar-fill { background: #ef4444; }
  .rd-transfer__error { margin: 6px 0 0 0; font-size: 12px; color: #fca5a5; }
  .rd-transfer__done { margin: 6px 0 0 0; font-size: 12px; color: #4ade80; }
  .rd-transfer__done--warn { color: #fbbf24; }
  .rd-transfer__where {
    margin: 6px 0 0 0;
    padding: 8px 10px;
    background: rgba(56, 189, 248, 0.08);
    border: 1px solid rgba(56, 189, 248, 0.2);
    border-radius: 6px;
    font-size: 12px;
    color: #cbd5e1;
    line-height: 1.5;
  }
  .rd-transfer__where code {
    display: inline-block;
    margin-top: 4px;
    padding: 4px 8px;
    background: #0a0f15;
    border-radius: 4px;
    color: #38bdf8;
    font-size: 11px;
  }
  .rd-transfer__copy {
    margin-left: 8px;
    background: transparent;
    border: 1px solid #1f2a36;
    color: #94a3b8;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    cursor: pointer;
  }
  .rd-transfer__copy:hover {
    background: #1f2a36;
    color: #fff;
  }
  .rd-transfer__close {
    position: absolute;
    top: 6px; right: 8px;
    width: 22px; height: 22px;
    background: transparent;
    border: none;
    color: #64748b;
    font-size: 18px;
    cursor: pointer;
    border-radius: 4px;
    line-height: 1;
  }
  .rd-transfer__close:hover { background: rgba(239, 68, 68, 0.15); color: #fca5a5; }

  /* Responsive : sur petit écran on cache le label, on garde l'icône */
  @media (max-width: 600px) {
    .rd-viewer__fab-label { display: none; }
    .rd-viewer__fab { padding: 8px 10px; }
  }

  /* En plein écran natif, le stage prend tout l'écran et la vidéo s'étire */
  .rd-viewer__stage:fullscreen,
  .rd-viewer__stage:-webkit-full-screen {
    aspect-ratio: auto;
    width: 100vw;
    height: 100vh;
    border-radius: 0;
    border: none;
    background: #000;
  }
  .rd-viewer__stage:fullscreen .rd-viewer__video,
  .rd-viewer__stage:-webkit-full-screen .rd-viewer__video {
    width: 100%;
    height: 100%;
  }

  .rd-viewer__stage {
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 12px;
    overflow: hidden;
    aspect-ratio: 16 / 9;
    display: flex;
    align-items: center;
    justify-content: center;
    position: relative;
  }
  .rd-viewer__video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    background: #000;
    outline: none;
    cursor: default;
  }
  .rd-viewer__video.active {
    cursor: crosshair;
    outline: 2px solid rgba(56, 189, 248, 0.45);
    outline-offset: -2px;
  }
  .rd-viewer__placeholder {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
    color: #94a3b8;
    font-size: 13px;
  }

  /* ── Menu post-connexion (3 cartes) ─────────────────────────────── */
  .rd-session-menu__head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 18px;
    flex-wrap: wrap;
  }
  .rd-features {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 14px;
  }
  @media (max-width: 720px) {
    .rd-features { grid-template-columns: 1fr; }
  }
  .rd-feature {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 12px;
    padding: 22px 20px;
    cursor: pointer;
    text-align: left;
    color: #e2e8f0;
    transition: background 0.15s, border-color 0.15s, transform 0.15s;
  }
  .rd-feature:hover {
    background: #0f1620;
    border-color: rgba(56, 189, 248, 0.4);
    transform: translateY(-1px);
  }
  .rd-feature__icon {
    font-size: 28px;
    line-height: 1;
    width: 48px; height: 48px;
    border-radius: 10px;
    background: rgba(56, 189, 248, 0.1);
    display: flex;
    align-items: center;
    justify-content: center;
    margin-bottom: 4px;
  }
  .rd-feature strong { font-size: 16px; color: #fff; }
  .rd-feature__hint { font-size: 12px; color: #94a3b8; }

  /* ── Bouton Play central (overlay quand pas encore Play) ────────── */
  .rd-viewer__big-play {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    background: rgba(56, 189, 248, 0.15);
    border: 1px solid rgba(56, 189, 248, 0.5);
    border-radius: 16px;
    padding: 22px 36px;
    color: #fff;
    font-size: 15px;
    cursor: pointer;
    transition: background 0.15s, transform 0.15s;
  }
  .rd-viewer__big-play:hover {
    background: rgba(56, 189, 248, 0.25);
    transform: scale(1.03);
  }
  .rd-viewer__big-play-icon { font-size: 36px; line-height: 1; }

  .rd-viewer__fab--accent {
    background: rgba(56, 189, 248, 0.18);
    border-color: rgba(56, 189, 248, 0.5);
    color: #38bdf8;
  }
  .rd-viewer__fab--accent:hover:not(:disabled) {
    background: rgba(56, 189, 248, 0.3);
    color: #fff;
  }

  /* ═══════════════════════════════════════════════════════════════
     Styles legacy (utilisés uniquement par le bloc {#if false}) —
     conservés pour ne pas casser le code commenté.
     ═══════════════════════════════════════════════════════════════ */
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

  .hero {
    align-items: flex-end;
    padding: 6px 0 2px;
  }

  .hero-copy {
    display: grid;
    gap: 8px;
    max-width: 720px;
  }

  .eyebrow {
    margin: 0;
    text-transform: uppercase;
    letter-spacing: 0.18em;
    font-size: 0.72rem;
    color: #7dd3fc;
  }

  .hero-text {
    margin: 0;
    color: #cbd5e1;
    max-width: 62ch;
    line-height: 1.5;
  }

  .status-strip {
    justify-content: flex-end;
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
    border-radius: 16px;
    padding: 16px;
    backdrop-filter: blur(8px);
    box-shadow: 0 18px 38px rgba(2, 6, 23, 0.18);
  }

  .big {
    font-size: 2.15rem;
    margin-top: 12px;
    margin-bottom: 0;
    font-weight: 700;
    letter-spacing: -0.03em;
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
    padding: 5px 10px;
    border-radius: 999px;
    border: 1px solid rgba(148, 163, 184, 0.35);
    font-size: 0.8rem;
    background: rgba(15, 23, 42, 0.36);
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

  .pill.error {
    border-color: rgba(248, 113, 113, 0.45);
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

  .metric-card {
    background: linear-gradient(180deg, rgba(15, 23, 42, 0.92), rgba(15, 23, 42, 0.72));
  }

  .metrics-panel {
    display: grid;
    gap: 12px;
  }

  .metrics-summary {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    text-align: left;
    background: transparent;
    border: 0;
    padding: 0;
  }

  .metric-tile {
    border: 1px solid rgba(148, 163, 184, 0.14);
    border-radius: 14px;
    padding: 16px;
  }

  .metric-tile h3 {
    margin: 0;
    font-size: 1rem;
    color: #cbd5e1;
  }

  .agent-item {
    background: linear-gradient(180deg, rgba(15, 23, 42, 0.48), rgba(15, 23, 42, 0.24));
  }

  .agent-meta {
    display: grid;
    gap: 4px;
  }

  .session-card {
    display: grid;
    gap: 12px;
  }

  .session-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    gap: 12px;
  }

  .session-kv {
    display: grid;
    gap: 6px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid rgba(148, 163, 184, 0.16);
    background: rgba(15, 23, 42, 0.46);
  }

  .session-kv-label {
    color: #94a3b8;
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .session-toolbar {
    padding-top: 2px;
  }

  .connection-code-card {
    display: grid;
    gap: 12px;
    padding: 14px;
    border: 1px solid rgba(148, 163, 184, 0.16);
    border-radius: 14px;
    background: rgba(15, 23, 42, 0.42);
  }

  .connection-code-row {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }

  .connection-code-value {
    display: inline-flex;
    align-items: center;
    min-height: 48px;
    padding: 0 16px;
    border-radius: 12px;
    border: 1px solid rgba(125, 211, 252, 0.22);
    background: rgba(15, 23, 42, 0.75);
    color: #e0f2fe;
    font-size: 1.35rem;
    font-weight: 700;
    letter-spacing: 0.08em;
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

  .remote-session-card {
    display: grid;
    gap: 14px;
  }

  .remote-session-card.expanded {
    max-width: min(1500px, calc(100vw - 32px));
  }

  .remote-session-heading {
    display: grid;
    gap: 4px;
  }

  .remote-session-actions {
    justify-content: flex-end;
  }

  .viewer-status-bar,
  .viewer-status-stack {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }

  .viewer-status-bar {
    flex-direction: column;
    align-items: flex-start;
  }

  .viewer-status-summary {
    width: 100%;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(210px, 1fr));
    gap: 10px;
  }

  .viewer-summary-tile {
    display: grid;
    gap: 8px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid rgba(148, 163, 184, 0.14);
    background: rgba(15, 23, 42, 0.42);
  }

  .control-hint {
    margin-top: 0;
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

  .remote-session-card.expanded .video-shell {
    min-height: 72vh;
  }

  .video-shell:fullscreen {
    border-radius: 0;
    border: 0;
    min-height: 100vh;
    background: #020617;
  }

  .video-shell:fullscreen .viewer-video,
  .video-shell:fullscreen .screen-preview {
    max-height: 100vh;
    height: 100vh;
  }

  .remote-toolbar {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 2;
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    padding: 14px;
    background: linear-gradient(180deg, rgba(2, 6, 23, 0.94), rgba(2, 6, 23, 0.35) 72%, rgba(2, 6, 23, 0));
    opacity: 0;
    transform: translateY(-10px);
    transition: opacity 0.2s ease, transform 0.2s ease;
    pointer-events: none;
  }

  .remote-toolbar.visible {
    opacity: 1;
    transform: translateY(0);
  }

  .viewer-toolbar-group {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .viewer-toolbar-status {
    pointer-events: none;
    max-width: min(72%, 860px);
  }

  .viewer-toolbar-actions {
    pointer-events: auto;
    margin-left: auto;
    justify-content: flex-end;
  }

  .telemetry-pill {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 34px;
    padding: 6px 12px;
    border-radius: 999px;
    border: 1px solid rgba(148, 163, 184, 0.24);
    background: rgba(15, 23, 42, 0.74);
    color: #e2e8f0;
    font-size: 0.82rem;
  }

  .telemetry-pill strong {
    font-size: 0.92rem;
    letter-spacing: 0.01em;
  }

  .telemetry-label {
    color: #94a3b8;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .telemetry-dot {
    width: 8px;
    height: 8px;
    border-radius: 999px;
    background: currentColor;
    box-shadow: 0 0 0 3px rgba(148, 163, 184, 0.12);
  }

  .toolbar-btn {
    padding: 6px 10px;
    min-height: 34px;
    border-radius: 999px;
    background: rgba(15, 23, 42, 0.72);
    border: 1px solid rgba(148, 163, 184, 0.24);
    pointer-events: auto;
  }

  .danger-ghost {
    border-color: rgba(248, 113, 113, 0.42);
    color: #fecaca;
    background: rgba(127, 29, 29, 0.18);
  }

  .viewer-video {
    width: 100%;
    height: auto;
    min-height: 320px;
    display: block;
    background: #020617;
    object-fit: contain;
  }

  .viewer-video.active {
    cursor: crosshair;
  }

  .viewer-video:focus-visible {
    outline: 2px solid rgba(96, 165, 250, 0.9);
    outline-offset: -2px;
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

  .debug-panel {
    border: 1px solid rgba(148, 163, 184, 0.18);
    border-radius: 12px;
    background: rgba(15, 23, 42, 0.45);
    overflow: hidden;
  }

  .debug-panel summary {
    cursor: pointer;
    padding: 12px 14px;
    color: #cbd5e1;
    font-weight: 600;
    list-style: none;
  }

  .debug-panel summary::-webkit-details-marker {
    display: none;
  }

  .debug-empty {
    padding: 0 14px 14px;
  }

  .signal-log {
    display: grid;
    gap: 6px;
    padding: 12px 14px;
    border-top: 1px solid rgba(148, 163, 184, 0.12);
  }

  .signal-log-head {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    flex-wrap: wrap;
  }

  .signal-log-payload {
    white-space: pre-wrap;
    word-break: break-word;
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

    .hero {
      align-items: flex-start;
    }

    .status-strip {
      justify-content: flex-start;
    }

    input {
      min-width: 0;
      width: 100%;
    }

    .approval-modal {
      padding: 24px;
    }

    .remote-toolbar {
      flex-direction: column;
      align-items: flex-start;
    }

    .remote-session-actions {
      justify-content: flex-start;
    }
  }

  /* â”€â”€ File transfer UI â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ */
  h3 {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    color: #cbd5e1;
  }

  .file-input {
    flex: 1;
    font-size: 0.85rem;
    color: #e2e8f0;
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(148,163,184,0.25);
    border-radius: 8px;
    padding: 6px 10px;
    cursor: pointer;
  }

  .file-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .file-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    flex: 1;
    overflow: hidden;
  }

  .file-icon {
    font-size: 1rem;
    flex-shrink: 0;
  }

  .file-name {
    font-size: 0.9rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 340px;
  }

  .btn-sm {
    font-size: 0.78rem;
    padding: 4px 10px;
    background: rgba(255,255,255,0.06);
    border: 1px solid rgba(148,163,184,0.25);
    border-radius: 6px;
    color: #e2e8f0;
    cursor: pointer;
    white-space: nowrap;
  }

  .btn-sm:hover {
    background: rgba(255,255,255,0.12);
  }

  .btn-sm:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .transfer-item {
    display: block;
    padding: 10px 14px;
  }

  .transfer-meta {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    margin-bottom: 6px;
  }

  .transfer-direction {
    font-size: 0.78rem;
    font-weight: 700;
    color: #7dd3fc;
  }

  .progress-bar-wrap {
    width: 100%;
    height: 6px;
    background: rgba(255,255,255,0.08);
    border-radius: 4px;
    overflow: hidden;
    margin-bottom: 4px;
  }

  .progress-bar-fill {
    height: 100%;
    background: #38bdf8;
    border-radius: 4px;
    transition: width 0.2s ease;
  }

  /* â”€â”€ Chat â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ */
  .chat-list {
    max-height: 360px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .chat-item {
    display: block;
    padding: 8px 12px;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.04);
  }

  .chat-item.chat-self {
    background: rgba(56, 189, 248, 0.08);
    align-self: flex-end;
    max-width: 80%;
  }

  .chat-bubble {
    margin: 0;
    line-height: 1.4;
    word-break: break-word;
  }

  .chat-ts {
    margin-top: 2px;
    font-size: 0.72rem;
  }

  .chat-typing {
    font-style: italic;
    color: #94a3b8;
  }

  .chat-input {
    flex: 1;
  }
</style>





