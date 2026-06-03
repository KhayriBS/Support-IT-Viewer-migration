<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import { technicianApi } from "$lib/api";
  import type { Agent, ControlSession, SignalMessage } from "$lib/api";
  import type { FileTransfer } from "$lib/api/types";
  import { diag } from "$lib/utils/diag";
  import {
    rdFormatDuration,
    rdFormatTime,
    rdFormatRelative,
    rdFormatBytes,
    rdFileIconClass,
    formatFileSize,
    formatBytesApprox
  } from "$lib/utils/format";
  import { transferProgress, transferSpeed } from "$lib/utils/transfer";
  import {
    viewerStateClass,
    viewerStateLabel,
    viewerQualityClass,
    viewerQualityLabel,
    statusClass
  } from "$lib/utils/viewer";
  import RdSessionHistory from "$lib/components/RdSessionHistory.svelte";
  import RdFileHistory from "$lib/components/RdFileHistory.svelte";
  import RdViewerStatsBar from "$lib/components/RdViewerStatsBar.svelte";
  import RdAppHeader from "$lib/components/RdAppHeader.svelte";
  import RdConnectPanel from "$lib/components/RdConnectPanel.svelte";
  import RdSessionMenu from "$lib/components/RdSessionMenu.svelte";
  import RdMetricsPanel from "$lib/components/RdMetricsPanel.svelte";
  import RdChatList from "$lib/components/RdChatList.svelte";
  import RdChatCompose from "$lib/components/RdChatCompose.svelte";
  import RdTransferList from "$lib/components/RdTransferList.svelte";
  import RdFilesPanel from "$lib/components/RdFilesPanel.svelte";
  import RdChatPanel from "$lib/components/RdChatPanel.svelte";
  import RdScreenPanel from "$lib/components/RdScreenPanel.svelte";
  import RdSupervisedMachines from "$lib/components/RdSupervisedMachines.svelte";
  import RdUsersPanel from "$lib/components/RdUsersPanel.svelte";
  import RdDashboardCards, { type DashCard } from "$lib/components/RdDashboardCards.svelte";
  import PrivacyControl from "$lib/components/PrivacyControl.svelte";
  import { agentManager } from "$lib/managers/agent-manager.svelte";
  import { approvalManager } from "$lib/managers/approval-manager.svelte";
  import { historyManager } from "$lib/managers/history-manager.svelte";
  import { signalBus } from "$lib/managers/signal-bus.svelte";
  import { viewerPeer } from "$lib/managers/viewer-peer.svelte";
  import { fileChannel } from "$lib/managers/file-channel.svelte";
  import { aiPipeline } from "$lib/managers/ai-pipeline.svelte";
  import { chatManager } from "$lib/managers/chat-manager.svelte";
  import { sessionManager } from "$lib/managers/session-manager.svelte";
  import type { RdFileRow } from "$lib/types/ui";

  // AgentMetrics & lifecycle state → cf. $lib/managers/agent-manager.svelte.ts

  let onlineAgents = $state<Agent[]>([]);
  let agentsLoading = $state(false);
  let agentsError = $state<string | null>(null);
  let agentsUpdatedAt = $state<string>("-");

  let connectionCode = $state("");
  /** Carte sélectionnée. null = grille d'accueil. */
  let dashView = $state<DashCard | null>(null);
  function goCard(c: DashCard) { dashView = c; }
  function backToCards() { dashView = null; }
  // SignalBus (signaling state/logs/reconnect) → cf. $lib/managers/signal-bus.svelte.ts
  // Historique sessions/fichiers → cf. $lib/managers/history-manager.svelte.ts

  const uiDebugEnabled =
    import.meta.env.DEV &&
    String((import.meta as unknown as { env?: Record<string, unknown> }).env?.VITE_UI_DEBUG ?? "") === "1";
  let backendSessionSynced = $state(false);
  let backendSyncError = $state<string | null>(null);
  let detachViewerInputListeners: (() => void) | null = null;
  let detachViewerFullscreenListener: (() => void) | null = null;
  // ViewerPeer (RTCPeerConnection + stream + input + stats) → cf. $lib/managers/viewer-peer.svelte.ts

  let metricsTimer: ReturnType<typeof setInterval>;
  let agentsTimer: ReturnType<typeof setInterval>;
  // Session approval modal
  let machineId = $state<string>("");
  // Machine id / connection code state → cf. agent-manager.svelte.ts
  // Approval modal state + polling sont gérés par le +layout.svelte parent.

  // Debounce des fetchs d'historique sur changement de filtres/clé machine.
  $effect(() => {
    void agentManager.localConnectionCode;
    void agentManager.localMachineId;
    void historyManager.sessionTypeFilter;
    void historyManager.sessionStatusFilter;
    void historyManager.sessionSearch;
    historyManager.scheduleSessionsRefresh();
  });

  $effect(() => {
    void agentManager.localConnectionCode;
    void agentManager.localMachineId;
    void historyManager.fileFilter;
    void historyManager.fileSearch;
    historyManager.scheduleFilesRefresh();
  });

  // Refresh immédiat quand une session vient de démarrer / se terminer.
  $effect(() => {
    void sessionManager.activeSession?.id;
    void sessionManager.activeSession?.status;
    void approvalManager.pendingSession?.id;
    void historyManager.fetchSessions();
    void historyManager.fetchFiles();
  });

  // Refresh quand un transfert se termine localement.
  $effect(() => {
    void Object.keys(fileChannel.fileTransfers).length;
    for (const t of Object.values(fileChannel.fileTransfers)) void t.state;
    historyManager.scheduleFilesRefresh(400);
  });

  // Auto-scroll de la liste de chat vers le bas dès qu'un message arrive
  // (ou que la liste change de sujet). On lit chatManager.chatMessages.length pour la
  // réactivité, puis on défile le conteneur en bas.
  $effect(() => {
    void chatManager.chatMessages.length;
    const el = chatManager.chatListEl;
    if (!el) return;
    queueMicrotask(() => { el.scrollTop = el.scrollHeight; });
  });

  // ── Listes filtrées dérivées ──
  // Les sessions sont déjà filtrées côté backend, on les affiche telles quelles.
  // (La recherche/filtre côté client est conservée comme fallback en attendant
  // que le fetch debounced revienne.)
  const rdFilteredSessions = $derived(historyManager.sessions);

  /**
   * Vue normalisée d'un transfert pour l'historique. Vient soit de l'API
   * backend (audit BD persistant), soit du dictionnaire in-memory pour les
   * transferts en cours (qui n'ont pas encore d'enregistrement complet).
   */
  const rdFilteredFiles = $derived.by<RdFileRow[]>(() => {
    const search = historyManager.fileSearch.trim().toLowerCase();
    const rows = new Map<string, RdFileRow>();

    // 1) Source de vérité : historique BD via API
    for (const h of historyManager.files) {
      const peerLabel = h.peerLabel || (h.listDirection === "incoming" ? h.fromMachineId : h.toMachineId);
      const row: RdFileRow = {
        transferId: h.transferId,
        fileName: h.fileName,
        type: h.listDirection === "incoming" ? "download" : "upload",
        peerLabel,
        sizeBytes: h.fileSize,
        state:
          h.status === "COMPLETED" ? "complete"
            : h.status === "FAILED" ? "error"
            : h.status === "CANCELLED" ? "cancelled"
            : "active",
        error: h.errorMessage,
        startedMs: h.startedAt ? Date.parse(h.startedAt) : Date.now(),
        doneBytes: h.fileSize,
        isLive: false
      };
      rows.set(h.transferId, row);
    }

    // 2) Override avec les transferts in-flight (progression live, état temps réel)
    for (const t of Object.values(fileChannel.fileTransfers)) {
      const peer =
        sessionManager.activeSession?.agentMachineId
          ?? rows.get(t.transferId)?.peerLabel
          ?? "—";
      rows.set(t.transferId, {
        transferId: t.transferId,
        fileName: t.fileName,
        type: t.type,
        peerLabel: peer,
        sizeBytes: t.totalSize || t.doneBytes,
        state:
          t.state === "complete" ? "complete"
            : t.state === "error" ? "error"
            : "active",
        error: t.error ?? null,
        startedMs: t.startedAt,
        doneBytes: t.doneBytes,
        isLive: t.state === "active"
      });
    }

    return Array.from(rows.values())
      .filter((f) => {
        if (historyManager.fileFilter !== "all" && f.type !== historyManager.fileFilter) return false;
        if (search && !f.fileName.toLowerCase().includes(search)
            && !f.peerLabel.toLowerCase().includes(search)) return false;
        return true;
      })
      .sort((a, b) => b.startedMs - a.startedMs);
  });

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

  // AiPipeline (AI commands + screenshot capture + action forwarding) -> cf. $lib/managers/ai-pipeline.svelte.ts
  // Effet : rejoindre le topic /topic/ai/<sessionId> dès qu'une session est ACTIVE
  // et que le canal STOMP IA est connecté. C'est la voie principale (robuste)
  // pour recevoir les réponses Gemini, en plus de /user/queue/ai/actions.
  $effect(() => {
    const session = sessionManager.queriedSession ?? sessionManager.activeSession;
    const sid = session && session.status === "ACTIVE" ? String(session.id) : null;
    if (sid && aiPipeline.aiConnected) {
      aiPipeline.client.joinSession(sid);
    }
  });

  // â”€â”€ File DataChannel â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

  // ── Audit BD : log les transferts dans /file-transfers ────────────────────
  // Best-effort : on n'interrompt jamais le transfert si l'API échoue.

  /** Identifiant de la machine pair (l'autre côté). */
  $effect(() => {
    const videoEl = viewerPeer.viewerVideoEl;
    const stream = viewerPeer.viewerRemoteStream;

    if (!videoEl) {
      return;
    }

    const handleLoadedMetadata = () => {
      diag("video.loadedmetadata", { width: videoEl.videoWidth, height: videoEl.videoHeight });
      viewerPeer.syncViewerVideoMetadata(videoEl);
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
      viewerPeer.syncViewerVideoMetadata(videoEl);
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

  async function handleIncomingSignal(message: SignalMessage) {
    // LEAVE coordonne plusieurs managers (signal + viewer + chat + backend) — orchestrateur.
    if (message.type === "LEAVE") {
      signalBus.remoteEnded = true;
      signalBus.manualDisconnect = true;
      signalBus.stopReconnect();
      viewerPeer.stopViewerOfferRetry();
      signalBus.signalingError = "Session terminee par le poste distant.";
      viewerPeer.screenFrameError = "Le poste distant a ferme la session.";
      signalBus.client.disconnect();
      signalBus.clearListeners();
      viewerPeer.resetViewerPeerConnection();
      chatManager.disconnect();
      signalBus.signalingConnected = false;
      if (backendSessionSynced) {
        await leaveBackendSession();
      } else {
        backendSyncError = null;
      }
      return;
    }

    // ERROR / STREAM_STATS / ANSWER / ICE → ViewerPeer
    await viewerPeer.handleIncomingViewerSignal(message);
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
    const currentMachineId = agentManager.localMachineId.trim();
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


  // ── Contrôle d'émission de frames côté agent (Pause / Play / transfert) ──
  // L'agent démarre PAUSÉ. On lui envoie VIDEO_RESUME / VIDEO_PAUSE via le
  // DataChannel "input" (P2P, reste ouvert même quand le signaling WebSocket
  // est fermé par Render avec un 1003 après l'OFFER/ANSWER).
  let rdFileInputEl = $state<HTMLInputElement | null>(null);
  let rdVideoPausedForTransfer = $state(false);
  // Préférence utilisateur : true = on VEUT voir l'écran (Play). Combiné avec
  // l'état des transferts, ça donne l'état réel envoyé à l'agent.
  let rdScreenPlayRequested = $state(false);

  function rdSendVideoControl(paused: boolean) {
    const payload = JSON.stringify({ type: paused ? "VIDEO_PAUSE" : "VIDEO_RESUME" });
    // 1) Voie principale : DataChannel "input" (P2P, toujours ouvert si peer Connected)
    try {
      if (viewerPeer.viewerControlChannel && viewerPeer.viewerControlChannel.readyState === "open") {
        viewerPeer.viewerControlChannel.send(payload);
      }
    } catch {
      /* canal momentanément KO */
    }
    // 2) Voie de secours : signaling, si encore connecté (utile au tout début
    //    avant l'ouverture du DataChannel input).
    try {
      if (signalBus.client.isConnected()) {
        signalBus.client.send({
          type: "STREAM_PROFILE",
          to: "agent",
          sessionId: sessionManager.activeSession ? String(sessionManager.activeSession.id) : undefined,
          payload: { profile: viewerPeer.viewerPlaybackProfile, paused }
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
    const sid = sessionManager.activeSession?.id ?? null;
    const isActive = sessionManager.activeSession?.status === "ACTIVE";
    const onScreen = sessionManager.selectedFeature === "screen";
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
    // Dépendances réactives explicites — y compris viewerPeer.viewerDataChannelOpen
    // pour que l'effet se redéclenche quand le canal devient utilisable
    // (et qu'on puisse ENFIN envoyer le PAUSE/RESUME désiré).
    void viewerPeer.viewerDataChannelOpen;
    void fileChannel.fileTransfers;

    const transferActive = fileChannel.hasActiveTransfer();
    rdVideoPausedForTransfer = transferActive;

    const onScreenTab = sessionManager.selectedFeature === "screen";
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
    if (!sessionManager.activeSession) {
      rdScreenPlayRequested = false;
      rdLastSentPaused = null;
      rdAutoPlayDoneForSession = null;
    }
  });

  function rdTriggerFilePicker() {
    // Source de vérité: l'état réel du canal au moment du clic, pas le state Svelte
    if (fileChannel.channel?.readyState !== "open") {
      console.warn("[file-ch] picker triggered but channel is", fileChannel.channel?.readyState);
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
        await fileChannel.uploadLocalFile(file);
      } catch (err) {
        console.error("[rd] upload failed:", err);
      }
    }
    // Reset pour pouvoir re-sélectionner le même fichier plus tard
    input.value = "";
  }

  // Polling fiable de l'état du DataChannel "file" — Svelte ne peut pas
  // observer fileChannel.channel?.readyState directement, donc on en fait un miroir
  // dans un $state régénéré à 500 ms.
  $effect(() => {
    const tick = () => {
      const open = fileChannel.channel?.readyState === "open";
      if (open !== fileChannel.rdFileChannelLive) fileChannel.rdFileChannelLive = open;
    };
    tick();
    const id = setInterval(tick, 500);
    return () => clearInterval(id);
  });

  function toggleViewerExpanded() {
    viewerPeer.viewerExpanded = !viewerPeer.viewerExpanded;
    viewerPeer.revealViewerControls();
  }

  async function connectSignaling(options?: { force?: boolean; reason?: string }) {
    const forceConnect = options?.force === true;
    const forceReason = options?.reason ?? null;
    diag("connectSignaling CALLED", {
      alreadyConnected: signalBus.signalingConnected,
      inFlight: signalBus.connectInFlight,
      force: forceConnect,
      reason: forceReason
    });
    console.trace("[DIAG] connectSignaling stack");

    if (signalBus.signalingConnected) {
      diag("connectSignaling SKIPPED â€” already connected");
      return;
    }
    if (signalBus.connectInFlight) {
      // Race guard: another connect attempt is already opening a WebSocket.
      // Without this, two parallel connects with the same token cause the
      // server to reject one of them with 1003.
      diag("connectSignaling SKIPPED â€” already in flight (race guard)");
      return;
    }
    signalBus.connectInFlight = true;

    const current = sessionManager.queriedSession ?? sessionManager.activeSession;
    if (!current) {
      signalBus.signalingError = "Demarrez ou chargez une session avant la connexion signaling.";
      signalBus.connectInFlight = false;
      return;
    }

    // Do not churn signaling sockets while media is already healthy.
    // Reconnect is only needed on-demand (e.g. ICE restart when media drops).
    const peerState = viewerPeer.viewerPeerConnection?.connectionState;
    if (!forceConnect && peerState === "connected") {
      diag("connectSignaling SKIPPED â€” peer already connected (background reconnect disabled)");
      signalBus.connectInFlight = false;
      return;
    }

    diag("connectSignaling using session", {
      id: current.id,
      status: current.status,
      tokenSuffix: current.signalingToken?.slice(-8)
    });

    signalBus.signalingError = null;
    signalBus.signalLogs = [];
    viewerPeer.viewerStreamMbps = null;
    viewerPeer.viewerStreamFps = null;
    viewerPeer.stopViewerAutoUpgradeTimer();
    signalBus.stopReconnect();
    viewerPeer.viewerProfileManualOverride = false;
    viewerPeer.viewerPlaybackProfile = "responsive";
    viewerPeer.revealViewerControls();
    signalBus.manualDisconnect = false;
    signalBus.remoteEnded = false;

    try {
      await signalBus.client.connect(current.signalingToken, "viewer", String(current.id));
      signalBus.reconnectAttempts = 0;
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
      signalBus.clearListeners();

      signalBus.detachMessageListener = signalBus.client.onMessage((message) => {
        signalBus.logSignal("in", message);
        viewerPeer.viewerSignalProcessing = viewerPeer.viewerSignalProcessing
          .then(() => handleIncomingSignal(message))
          .catch((error) => {
            if (uiDebugEnabled) {
              console.error("Signal processing failed", error);
            }
          });
      });

      signalBus.detachCloseListener = signalBus.client.onClose((event) => {
        const closeCode = event.code ?? 0;
        const peerState = viewerPeer.viewerPeerConnection?.connectionState;
        const iceState = viewerPeer.viewerPeerConnection?.iceConnectionState;
        diag("signaling SOCKET CLOSED", {
          code: closeCode,
          reason: event.reason,
          wasClean: event.wasClean,
          manualDisconnect: signalBus.manualDisconnect,
          remoteEnded: signalBus.remoteEnded,
          peerState,
          iceState
        });
        signalBus.signalingConnected = false;

        const isManualEnd = signalBus.manualDisconnect || signalBus.remoteEnded;
        const peerTerminal =
          peerState === "failed" || peerState === "closed";
        const peerAlreadyConnected = peerState === "connected";

        // Signaling can flap while media is still alive. Keep the peer when
        // possible and reconnect signaling in background so ICE restart remains
        // available if connectivity degrades later.

        if (isManualEnd || peerTerminal) {
          diag("signaling close â†’ RESETTING peer", { isManualEnd, peerTerminal });
          signalBus.stopReconnect();
          viewerPeer.resetViewerPeerConnection();
          if (closeCode === 1003) {
            signalBus.signalingError = "Signal ferme (1003): session/token invalide ou expire. Recharge la session.";
          } else if (closeCode === 1000) {
            signalBus.signalingError = "Signal ferme normalement (1000).";
          } else {
            signalBus.signalingError = `Signal ferme (code ${closeCode}).`;
          }
          if (backendSessionSynced) {
            void leaveBackendSession();
          }
          return;
        }
        if (peerAlreadyConnected) {
          diag("signaling close â†’ peer CONNECTED, keep media and reconnect signaling in background");
          signalBus.signalingError = null;
          // Keep media alive AND keep signaling reconnecting in the background:
          // we still need it for ICE restart, future OFFERs, chat and stats.
          // Reset the attempt counter so the backoff starts fresh each time
          // the peer is healthy (otherwise it drifts to the 10s ceiling).
          signalBus.reconnectAttempts = 0;
          signalBus.scheduleReconnect();
          return;
        }

        // Peer still negotiating (`new` / `connecting` / `checking`). Give it
        // a grace window to converge using the ICE candidates already on the
        // wire. If ICE doesn't reach `connected` in time, declare failure.
        diag("signaling close â†’ giving ICE a grace window to converge", {
          peerState,
          iceState
        });
        signalBus.signalingError = "Signal perdu — tentative de reprise signaling et ICE...";
        // Always keep retrying — previous logic stopped after the first
        // signaling drop once the peer had ever been connected, leaving
        // the viewer stranded with no way to reach the agent again.
        signalBus.scheduleReconnect();
        viewerPeer.startIceConvergenceWatchdog();
      });

      signalBus.detachErrorListener = signalBus.client.onError(() => {
        signalBus.signalingError = "Erreur socket signaling";
      });

      signalBus.signalingConnected = true;

      // Flush any ICE candidates that were generated while signaling was down
      // (often the critical TURN `relay` candidates that arrive late).
      if (signalBus.bufferedLocalIceCandidates.length > 0) {
        diag("flushing buffered ICE candidates", { count: signalBus.bufferedLocalIceCandidates.length });
        const toFlush = signalBus.bufferedLocalIceCandidates;
        signalBus.bufferedLocalIceCandidates = [];
        for (const ice of toFlush) {
          try {
            signalBus.client.send(ice, "viewer");
          } catch (err) {
            diag("flush ICE FAILED â€” re-buffering", String(err));
            signalBus.bufferedLocalIceCandidates.push(ice);
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
      signalBus.client.send(joinMessage, "viewer");
      signalBus.logSignal("out", { ...joinMessage, from: "viewer" });

      // CRITICAL: only send a fresh OFFER if we don't already have a working
      // peer. After a transient signaling close (1006/1011/1012/1013) the peer
      // is still alive and re-OFFERing would consume the token a second time
      // â†’ the server replies with 1003.
      const existingPeerState = viewerPeer.viewerPeerConnection?.connectionState;
      const peerAlreadyAlive =
        !!viewerPeer.viewerPeerConnection &&
        existingPeerState !== "closed" &&
        existingPeerState !== "failed";

      if (peerAlreadyAlive) {
        diag("connectSignaling: peer already alive â€” skipping re-OFFER", {
          peerState: existingPeerState
        });
      } else {
        await viewerPeer.sendViewerOffer(String(current.id));
      }
    } catch (error) {
      diag("connectSignaling THREW", String(error));
      signalBus.client.disconnect();
      signalBus.signalingConnected = false;
      const peerState = viewerPeer.viewerPeerConnection?.connectionState;
      const peerAlive =
        peerState === "connected" ||
        peerState === "connecting" ||
        peerState === "disconnected";
      if (!peerAlive || signalBus.manualDisconnect) {
        viewerPeer.resetViewerPeerConnection();
      } else {
        diag("connectSignaling failed but peer is still alive — keeping peer", { peerState });
      }
      if (signalBus.manualDisconnect) {
        backendSessionSynced = false;
        backendSyncError = null;
      }
      signalBus.signalingError = String(error);

      if (!signalBus.manualDisconnect) {
        // Always keep retrying so the signaling channel comes back even
        // after the peer has been healthy at some point — needed for
        // ICE restart / chat / stats if connectivity later degrades.
        signalBus.scheduleReconnect();
      }
    } finally {
      // Always release the in-flight lock so the next legitimate connect attempt
      // (e.g. scheduled reconnect) can proceed.
      signalBus.connectInFlight = false;
    }
  }

  async function disconnectSignaling(options?: { sendLeave?: boolean }) {
    diag("disconnectSignaling CALLED", { sendLeave: options?.sendLeave === true });
    // Stack trace so we see exactly which caller fired this â€” Svelte effect,
    // onDestroy, button click, error handler, etc.
    console.trace("[DIAG] disconnectSignaling stack");

    signalBus.manualDisconnect = true;
    signalBus.stopReconnect();

    const shouldSendLeave = options?.sendLeave === true;
    const current = sessionManager.queriedSession ?? sessionManager.activeSession;
    if (shouldSendLeave && signalBus.client.isConnected() && current?.id) {
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
        signalBus.client.send(leaveMessage, "viewer");
      } catch (err) {
        diag("LEAVE send failed", String(err));
      }
    }

    signalBus.client.disconnect();
    viewerPeer.resetViewerPeerConnection();
    signalBus.clearListeners();
    signalBus.signalingConnected = false;
    signalBus.reconnectAttempts = 0;
    if (backendSessionSynced) {
      await leaveBackendSession();
    } else {
      backendSyncError = null;
    }
  }


  // Le callback approvalManager.onApproved est posé par +layout.svelte
  // car il doit aussi fonctionner sur les routes /my-machines et /pending.

  onMount(() => {
    sessionManager.connectSignaling = () => connectSignaling();
    sessionManager.disconnectSignaling = (opts) => disconnectSignaling(opts);
    sessionManager.getConnectionCode = () => connectionCode;

    signalBus.shouldReconnect = () => {
      const current = sessionManager.queriedSession ?? sessionManager.activeSession;
      return !!current && current.status === "ACTIVE";
    };
    signalBus.doReconnect = () => {
      void connectSignaling();
    };

    viewerPeer.getSession = () => sessionManager.queriedSession ?? sessionManager.activeSession;
    viewerPeer.getSelectedFeature = () => sessionManager.selectedFeature;
    viewerPeer.connectSignaling = (opts) => connectSignaling(opts);
    viewerPeer.disconnectChat = () => chatManager.disconnect();
    viewerPeer.leaveBackendSession = () => leaveBackendSession();
    viewerPeer.isBackendSessionSynced = () => backendSessionSynced;
    viewerPeer.clearBackendSyncError = () => { backendSyncError = null; };
    viewerPeer.configureFileDataChannel = (channel) => fileChannel.configure(channel);
    viewerPeer.resetFileChannel = () => fileChannel.reset();
    viewerPeer.handleAiActionResult = (payload) => aiPipeline.handleAiActionResult(payload);
    viewerPeer.handleScreenshotResponse = (payload) => aiPipeline.handleScreenshotResponse(payload);
    viewerPeer.onControlChannelOpen = () => { rdLastSentPaused = null; };

    fileChannel.getSession = () => sessionManager.activeSession ?? sessionManager.queriedSession;
    fileChannel.getChatLocalRole = () => chatManager.chatLocalRole;
    fileChannel.getLocalMachineId = () => agentManager.localMachineId;

    aiPipeline.getSession = () => sessionManager.queriedSession ?? sessionManager.activeSession;
    aiPipeline.getSelectedFeature = () => sessionManager.selectedFeature;
    aiPipeline.getChatLocalRole = () => chatManager.chatLocalRole;
    aiPipeline.getChatRemoteRole = () => chatManager.chatRemoteRole;
    aiPipeline.getChatRoomId = () => chatManager.chatRoomId;
    aiPipeline.resolveRoomId = () => chatManager.resolveRoomId();
    aiPipeline.pushChatMessage = (msg) => { chatManager.chatMessages = [...chatManager.chatMessages, msg].slice(-200); };

    chatManager.getSession = () => sessionManager.queriedSession ?? sessionManager.activeSession;
    chatManager.getSelectedFeature = () => sessionManager.selectedFeature;
    chatManager.isLocalAgentTargeted = (s) => shouldBridgeSessionToLocalAgent(s);
    aiPipeline.getChatInput = () => chatManager.chatInput;
    aiPipeline.setChatInput = (v) => { chatManager.chatInput = v; };
    aiPipeline.requestVideoResume = () => { rdScreenPlayRequested = true; };
    aiPipeline.isVideoPlayRequested = () => rdScreenPlayRequested;

    const handleKeyDown = (event: KeyboardEvent) => viewerPeer.handleViewerDocumentKeyDown(event);
    const handleKeyUp = (event: KeyboardEvent) => viewerPeer.handleViewerDocumentKeyUp(event);
    const handleFullscreenChange = () => viewerPeer.syncViewerFullscreenState();
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

    // Lifecycle agent + approval polling vivent dans +layout.svelte
    // (cross-route). Ici on ne s'occupe que de la vue technicien.

    // Sanitize : après un échec de signaling (close 1011, peer abandonné),
    // waitingForApproval ou actionLoading peuvent rester collés à true et
    // bloquer l'input "Connexion par code". On les reset si plus aucune
    // session active.
    if (!sessionManager.activeSession) {
      sessionManager.waitingForApproval = false;
      sessionManager.actionLoading = false;
      sessionManager.actionError = null;
    }

    void viewerPeer.refreshViewerIceServers();
    agentManager.refreshMetrics();
    refreshOnlineAgents();

    metricsTimer = setInterval(agentManager.refreshMetrics, 2500);
    agentsTimer = setInterval(refreshOnlineAgents, 8000);
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

    // CRITICAL: only tear down the network sessions if the user is REALLY
    // leaving the app (window/tab close). Otherwise, a Svelte rerender,
    // route change, HMR, or component unmount would kill the active video
    // session â€” which is what was happening to you.
    if (!realUnloadInProgress) {
      diag("onDestroy SKIPPED network teardown â€” not a real unload");
      return;
    }

    diag("onDestroy: real teardown (window closing)");
    void disconnectSignaling();
    chatManager.disconnect(); // appelle aussi aiPipeline.disconnect() en cascade
    // agentManager.stopLifecycle() est appelé par +layout.svelte
  });
</script>

<svelte:head>
  <title>Lumiere IT | Dashboard</title>
  <meta name="description" content="Dashboard API migre depuis TechnicianViewer" />
</svelte:head>

<main class="rd-page">
  <section class="rd-card">
    <RdAppHeader />

    {#if sessionManager.activeSession && sessionManager.activeSession.status === "ACTIVE"}
      <!-- Session active : on garde l'UX historique (menu + sous-panneaux) -->
      {#if !sessionManager.selectedFeature}
        <RdSessionMenu
          session={sessionManager.activeSession}
          chatLocalRole={chatManager.chatLocalRole}
          actionLoading={sessionManager.actionLoading}
          peerLabel={
            sessionManager.activeSession.agentMachineId === agentManager.localMachineId
              ? (sessionManager.activeSession.technicianUsername || "le technicien")
              : sessionManager.activeSession.agentMachineId
          }
          onPickFeature={(f) => {
            if (f === "chat") sessionManager.chooseFeature("chat");
            else sessionManager.selectedFeature = f;
          }}
          onDisconnect={() => void sessionManager.stopByToken()} />
      {/if}

      {#if sessionManager.selectedFeature === "files"}
        <RdFilesPanel
          session={sessionManager.activeSession}
          actionLoading={sessionManager.actionLoading}
          bind:fileInputEl={rdFileInputEl}
          onBackToMenu={() => { sessionManager.selectedFeature = null; }}
          onTriggerPicker={rdTriggerFilePicker}
          onFilePicked={rdHandleFilePicked}
          onDisconnect={() => void sessionManager.stopByToken()} />
      {/if}

      {#if sessionManager.selectedFeature === "chat"}
        <RdChatPanel
          chatConnected={chatManager.chatConnected}
          chatError={chatManager.chatError}
          chatLocalRole={chatManager.chatLocalRole}
          messages={chatManager.chatMessages}
          typing={chatManager.typingInfo}
          bind:chatListEl={chatManager.chatListEl}
          bind:chatInput={chatManager.chatInput}
          actionLoading={sessionManager.actionLoading}
          composeDisabled={!sessionManager.activeSession || sessionManager.activeSession.status !== "ACTIVE"}
          onReconnect={() => void chatManager.connect()}
          onBackToMenu={() => { sessionManager.selectedFeature = null; }}
          onDisconnect={() => void sessionManager.stopByToken()}
          onSend={() => void chatManager.send()}
          onInput={chatManager.dispatchTyping} />
      {/if}

      {#if sessionManager.selectedFeature === "screen"}
        <div class="rd-privacy-row"><PrivacyControl /></div>
        <RdScreenPanel
          session={sessionManager.activeSession}
          actionLoading={sessionManager.actionLoading}
          chatConnected={chatManager.chatConnected}
          chatLocalRole={chatManager.chatLocalRole}
          messages={chatManager.chatMessages}
          typing={chatManager.typingInfo}
          bind:chatListEl={chatManager.chatListEl}
          bind:chatInput={chatManager.chatInput}
          {rdScreenPlayRequested}
          {rdVideoPausedForTransfer}
          bind:fileInputEl={rdFileInputEl}
          onBackToMenu={() => { sessionManager.selectedFeature = null; }}
          onPlay={rdPlayScreen}
          onPause={rdPauseScreen}
          onConnectChat={() => void chatManager.connect()}
          onTriggerFilePicker={rdTriggerFilePicker}
          onFilePicked={rdHandleFilePicked}
          onSendMessage={() => void chatManager.send()}
          onDispatchTyping={chatManager.dispatchTyping}
          onDisconnect={() => void sessionManager.stopByToken()} />
      {/if}
    {:else if dashView === null}
      <!-- Accueil : connexion par code + grille de cartes -->
      <RdConnectPanel
        bind:connectionCode
        actionLoading={sessionManager.actionLoading}
        waitingForApproval={sessionManager.waitingForApproval}
        actionError={sessionManager.actionError}
        onConnect={() => void sessionManager.startSessionWithCode()} />
      <RdDashboardCards onPick={goCard} />
    {:else}
      <!-- Vue détail d'une carte -->
      <div class="rd-back-row">
        <button class="rd-back" type="button" onclick={backToCards}>← Retour</button>
        <h2 class="rd-back__title">
          {#if dashView === "me"}Ma machine
          {:else if dashView === "machines"}Machines supervisées
          {:else if dashView === "users"}Utilisateurs
          {:else if dashView === "history"}Historiques
          {/if}
        </h2>
      </div>

      {#if dashView === "me"}
        <RdMetricsPanel />
      {:else if dashView === "machines"}
        <RdSupervisedMachines />
      {:else if dashView === "users"}
        <RdUsersPanel />
      {:else if dashView === "history"}
        <div class="rd-history-grid">
          <RdSessionHistory
            entries={rdFilteredSessions}
            error={historyManager.sessionsError}
            loading={historyManager.sessionsLoading}
            bind:search={historyManager.sessionSearch}
            bind:typeFilter={historyManager.sessionTypeFilter}
            bind:statusFilter={historyManager.sessionStatusFilter} />
          <RdFileHistory
            entries={rdFilteredFiles}
            error={historyManager.filesError}
            loading={historyManager.filesLoading}
            bind:search={historyManager.fileSearch}
            bind:filter={historyManager.fileFilter} />
        </div>
      {/if}
    {/if}
  </section>
</main>

<style>
  .rd-back-row {
    display: flex;
    align-items: center;
    gap: 16px;
    margin: 8px 0 18px;
  }
  .rd-back {
    background: rgba(255, 255, 255, 0.06);
    color: inherit;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    padding: 8px 14px;
    font-size: 13px;
    cursor: pointer;
  }
  .rd-back:hover { background: rgba(255, 255, 255, 0.12); }
  .rd-back__title {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    opacity: 0.9;
  }
</style>
