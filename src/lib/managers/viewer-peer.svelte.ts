import { invoke } from "@tauri-apps/api/core";
import { technicianApi } from "$lib/api";
import type { ControlSession, SignalMessage } from "$lib/api";
import { defaultViewerIceServers, isEditableTarget, resolveIceServers } from "$lib/utils/viewer";

// Préfixes ICE qu'on refuse — VPN WireGuard/NordLynx, VirtualBox Host-Only,
// VMware VMnet1/8, APIPA, IPv6 link-local. Même liste que le côté Rust
// (src-tauri/src/agent/network.rs::BLOCKED_IP_PREFIXES). À garder en
// synchro si tu ajoutes une nouvelle interface virtuelle.
const BLOCKED_IP_PREFIXES = [
  "10.5.", "10.6.",
  "192.168.56.",
  "192.168.30.", "192.168.9.",
  "169.254.",
  "fe80:", "fe80::"
];

function extractCandidateIp(line: string | null | undefined): string | null {
  if (!line) return null;
  const stripped = line.startsWith("candidate:") ? line.slice("candidate:".length) : line;
  const parts = stripped.split(/\s+/);
  return parts.length >= 5 ? parts[4] : null;
}

/** false si l'IP du candidat tombe dans BLOCKED_IP_PREFIXES (VPN/virtuel/APIPA). */
function isValidCandidate(candidate: RTCIceCandidate): boolean {
  const ip = (candidate.address ?? extractCandidateIp(candidate.candidate))?.toLowerCase();
  if (!ip) return true; // pas d'IP parsable (mDNS .local) — on laisse passer
  return !BLOCKED_IP_PREFIXES.some((prefix) => ip.startsWith(prefix.toLowerCase()));
}
import { diag } from "$lib/utils/diag";
import { signalBus } from "./signal-bus.svelte";
import {
  ICE_CONVERGENCE_WINDOW_MS,
  ICE_RESTART_ON_DISCONNECTED_DELAY_MS,
  ICE_RESTART_ON_FAILED_DELAY_MS,
  MAX_VIEWER_OFFER_RETRIES,
  streamProfileSignalEnabled,
  VIEWER_AUTO_UPGRADE_DELAY_MS,
  VIEWER_AUTO_UPGRADE_MIN_FPS,
  VIEWER_AUTO_UPGRADE_MIN_MBPS,
  VIEWER_MOUSE_MOVE_MIN_INTERVAL_MS,
  VIEWER_WHEEL_MIN_INTERVAL_MS,
} from "./viewer-peer.constants";
import type {
  RemoteInputEvent,
  ViewerBitrateTier,
  ViewerFpsTier,
  ViewerPlaybackProfile,
  ViewerPreset,
} from "./viewer-peer.types";
import {
  computeInboundSample,
  extractActiveRttMs,
  initialCounters,
  type CandidatePairLike,
  type InboundRtpVideoLike,
  type InboundStatsCounters,
} from "./viewer-stats";
import {
  formatTransportSummary,
  parseCandidateAddress,
  parseCandidatePort,
  parseCandidateType,
  readSelectedCandidatePair,
  type IceCandidateType,
} from "./viewer-ice-diag";

class ViewerPeer {
  // Reactive UI state
  viewerShellEl = $state<HTMLDivElement | null>(null);
  viewerVideoEl = $state<HTMLVideoElement | null>(null);
  viewerRemoteStream = $state<MediaStream | null>(null);
  viewerDataChannelOpen = $state(false);
  viewerKeyboardCaptured = $state(false);
  viewerConnectionState = $state<string>("idle");
  viewerControlsVisible = $state(true);
  viewerExpanded = $state(false);
  viewerFullscreenActive = $state(false);
  viewerChatPanelOpen = $state(false);
  viewerRemoteWidth = $state(1920);
  viewerRemoteHeight = $state(1080);
  viewerIceServers = $state<RTCIceServer[]>(defaultViewerIceServers);
  viewerStreamMbps = $state<number | null>(null);
  viewerStreamFps = $state<number | null>(null);
  viewerLocalFps = $state<number | null>(null);
  viewerLocalMbps = $state<number | null>(null);
  viewerLocalRttMs = $state<number | null>(null);
  viewerLocalLossPct = $state<number | null>(null);
  viewerLocalJitterMs = $state<number | null>(null);
  viewerLocalResolution = $state<string | null>(null);
  viewerLocalFramesDropped = $state<number | null>(null);
  viewerStatsBarVisible = $state(true);
  viewerPlaybackProfile = $state<ViewerPlaybackProfile>("responsive");
  viewerFpsTier = $state<ViewerFpsTier>("auto");
  viewerBitrateTier = $state<ViewerBitrateTier>("auto");
  viewerPreset = $state<ViewerPreset>("balanced");
  screenFrameError = $state<string | null>(null);
  viewerCandidatePairType = $state<"host" | "srflx" | "relay" | "prflx" | "unknown" | null>(null);
  viewerLocalCandidateAddress = $state<string | null>(null);
  viewerRemoteCandidateAddress = $state<string | null>(null);
  viewerTransportSummary = $state<string | null>(null);

  // Privacy filter — agent-side blur of password fields. `true` = blur ON.
  // Default-on; the technician must explicitly disable to view passwords.
  // The `privacy` RTCDataChannel is created alongside `input` / `file`;
  // its handle stays on the manager so PrivacyControl.svelte can flip
  // the toggle without traversing the component tree.
  viewerPrivacyBlurEnabled = $state<boolean>(true);
  viewerPrivacyChannelOpen = $state<boolean>(false);
  viewerPrivacyChannel: RTCDataChannel | null = null;

  // Non-reactive internals
  viewerPeerConnection: RTCPeerConnection | null = null;
  viewerControlChannel: RTCDataChannel | null = null;
  viewerSignalProcessing: Promise<void> = Promise.resolve();
  pendingViewerIceCandidates: RTCIceCandidateInit[] = [];
  viewerAnswerReceived = false;
  viewerHadConnectedOnce = false;
  viewerOfferRetryTimer: ReturnType<typeof setInterval> | null = null;
  viewerOfferRetryCount = 0;
  viewerControlsTimer: ReturnType<typeof setTimeout> | null = null;
  viewerProfileManualOverride = false;
  viewerProfileAutoUpgradeTimer: ReturnType<typeof setTimeout> | null = null;
  inboundStatsTimer: ReturnType<typeof setInterval> | null = null;
  iceWatchdogTimer: ReturnType<typeof setTimeout> | null = null;
  iceRestartTimer: ReturnType<typeof setTimeout> | null = null;
  iceRestartInFlight = false;

  // Input throttling
  lastViewerMouseMoveSentAt = 0;
  lastViewerWheelSentAt = 0;
  lastViewerPointerSent: { x: number; y: number } | null = null;

  // ── Callbacks set by the orchestrator (+page.svelte) ────────────────────
  /** Current control session (queriedSession ?? activeSession). */
  getSession: () => ControlSession | null = () => null;
  /** Currently selected feature ("screen" / "chat" / "files" / null). */
  getSelectedFeature: () => "screen" | "chat" | "files" | null = () => null;
  /** Force-reconnect signaling (used by ICE restart). */
  connectSignaling: (opts?: { force?: boolean; reason?: string }) => Promise<void> = async () => {};
  /** Tear down chat connection when the remote ends the session. */
  disconnectChat: () => void = () => {};
  /** Tell the local agent (Tauri) to leave the session. */
  leaveBackendSession: () => Promise<void> = async () => {};
  /** Whether the local agent has joined the backend session. */
  isBackendSessionSynced: () => boolean = () => false;
  /** Clear the "session synced with backend" flag (LEAVE path). */
  clearBackendSyncError: () => void = () => {};
  /** Attach FileChannel handlers to a freshly created "file" DataChannel. */
  configureFileDataChannel: (channel: RTCDataChannel) => void = () => {};
  /** Close the file channel and clear file transfer state. */
  resetFileChannel: () => void = () => {};
  /** Forward an AI_ACTION_RESULT message received over the control DataChannel. */
  handleAiActionResult: (payload: Record<string, unknown>) => void = () => {};
  /** Forward a screenshot_response (or chunked variant) received over the control DataChannel. */
  handleScreenshotResponse: (payload: Record<string, unknown>) => void = () => {};
  /** Reset the orchestrator's pause/resume tracking when the input channel re-opens. */
  onControlChannelOpen: () => void = () => {};

  // ── Stats logger ────────────────────────────────────────────────────────
  // Logique de calcul de delta extraite dans `viewer-stats.ts` (testable).
  // Ici on garde uniquement l'orchestration `setInterval` + propagation
  // vers les champs réactifs Svelte.
  startInboundStatsLogger = (pc: RTCPeerConnection) => {
    this.stopInboundStatsLogger();
    let counters: InboundStatsCounters = initialCounters(performance.now());

    this.inboundStatsTimer = setInterval(async () => {
      try {
        const stats = await pc.getStats();
        const now = performance.now();
        let foundInbound = false;
        const pairs: CandidatePairLike[] = [];

        stats.forEach((s) => {
          if (s.type === "inbound-rtp" && (s as RTCInboundRtpStreamStats).kind === "video") {
            foundInbound = true;
            const { sample, counters: nextCounters } = computeInboundSample(
              s as InboundRtpVideoLike,
              counters,
              now,
            );
            this.viewerLocalMbps = sample.mbps;
            this.viewerLocalFps = sample.fps;
            this.viewerLocalLossPct = sample.lossPct;
            this.viewerLocalJitterMs = sample.jitterMs;
            this.viewerLocalFramesDropped = sample.framesDropped;
            if (sample.resolution) {
              this.viewerLocalResolution = sample.resolution;
            }
            counters = nextCounters;
          }
          if (s.type === "candidate-pair") {
            pairs.push(s as CandidatePairLike);
          }
        });

        const rtt = extractActiveRttMs(pairs);
        if (rtt !== null) this.viewerLocalRttMs = rtt;

        if (!foundInbound) {
          this.viewerLocalMbps = 0;
          this.viewerLocalFps = 0;
          // Reset timestamp pour ne pas accumuler un delta géant si le stream
          // reprend après une longue pause.
          counters = { ...counters, timestampMs: now };
        }
      } catch (err) {
        diag("getStats failed", String(err));
      }
    }, 1000);
  };

  stopInboundStatsLogger = () => {
    if (this.inboundStatsTimer) {
      clearInterval(this.inboundStatsTimer);
      this.inboundStatsTimer = null;
    }
  };

  diagnoseLanConnectivity = async (
    pc?: RTCPeerConnection | null,
  ): Promise<string | null> => {
    const target = pc ?? this.viewerPeerConnection;
    if (!target) {
      this.viewerCandidatePairType = null;
      this.viewerLocalCandidateAddress = null;
      this.viewerRemoteCandidateAddress = null;
      this.viewerTransportSummary = null;
      return null;
    }
    try {
      const pair = await readSelectedCandidatePair(target);
      if (!pair) {
        this.viewerCandidatePairType = null;
        this.viewerLocalCandidateAddress = null;
        this.viewerRemoteCandidateAddress = null;
        this.viewerTransportSummary = "Aucune paire ICE nominée";
        return this.viewerTransportSummary;
      }
      const localStr = pair.local.address && pair.local.port
        ? `${pair.local.address}:${pair.local.port}`
        : pair.local.address;
      const remoteStr = pair.remote.address && pair.remote.port
        ? `${pair.remote.address}:${pair.remote.port}`
        : pair.remote.address;
      this.viewerCandidatePairType = pair.pairType as IceCandidateType;
      this.viewerLocalCandidateAddress = localStr;
      this.viewerRemoteCandidateAddress = remoteStr;
      this.viewerTransportSummary = formatTransportSummary(pair);
      console.info(
        `🧊 [ICE] selected pair type=${pair.pairType}  local=${localStr ?? "?"}  remote=${remoteStr ?? "?"}  → ${this.viewerTransportSummary}`,
      );
      if (pair.pairType === "relay") {
        console.warn(
          "🧊 [ICE] Le flux passe par TURN relay alors qu'un LAN direct était espéré. " +
            "Causes fréquentes : firewall qui filtre UDP entre les deux postes, " +
            "isolation client/AP (Wi-Fi guest), ou iceTransportPolicy='relay' côté config.",
        );
      }
      return this.viewerTransportSummary;
    } catch (err) {
      console.warn("🧊 [ICE] diagnoseLanConnectivity failed", err);
      this.viewerTransportSummary = `Diagnostic ICE échec : ${String(err)}`;
      return this.viewerTransportSummary;
    }
  };

  // ── ICE diagnostics ─────────────────────────────────────────────────────
  dumpIceCandidatePairs = async (pc: RTCPeerConnection) => {
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
  };

  startIceConvergenceWatchdog = () => {
    this.stopIceConvergenceWatchdog();
    this.iceWatchdogTimer = setTimeout(() => {
      this.iceWatchdogTimer = null;
      const pc = this.viewerPeerConnection;
      const state = pc?.connectionState;
      if (state === "connected") {
        diag("ICE watchdog: peer is connected, all good");
        signalBus.signalingError = null;
        return;
      }
      diag("ICE watchdog EXPIRED — peer never reached connected", { state });
      if (pc) void this.dumpIceCandidatePairs(pc);
      signalBus.signalingError =
        "Connexion video impossible apres perte du signaling. Recharge la session.";
      this.resetViewerPeerConnection();
      if (this.isBackendSessionSynced()) {
        void this.leaveBackendSession();
      }
    }, ICE_CONVERGENCE_WINDOW_MS);
  };

  stopIceConvergenceWatchdog = () => {
    if (this.iceWatchdogTimer) {
      clearTimeout(this.iceWatchdogTimer);
      this.iceWatchdogTimer = null;
    }
  };

  stopIceRestartTimer = () => {
    if (this.iceRestartTimer) {
      clearTimeout(this.iceRestartTimer);
      this.iceRestartTimer = null;
    }
  };

  scheduleIceRestart = (reason: string, delayMs: number) => {
    if (signalBus.manualDisconnect || signalBus.remoteEnded) {
      return;
    }
    if (!this.viewerPeerConnection || this.viewerPeerConnection.connectionState === "closed") {
      return;
    }
    if (this.iceRestartInFlight || this.iceRestartTimer) {
      return;
    }

    diag("ICE restart scheduled", {
      reason,
      delayMs,
      signalingConnected: signalBus.signalingConnected,
      peerState: this.viewerPeerConnection.connectionState,
      iceState: this.viewerPeerConnection.iceConnectionState
    });

    this.iceRestartTimer = setTimeout(() => {
      this.iceRestartTimer = null;
      void this.attemptIceRestart(reason);
    }, delayMs);
  };

  attemptIceRestart = async (reason: string) => {
    if (this.iceRestartInFlight) {
      return;
    }

    const session = this.getSession();
    if (!session || session.status !== "ACTIVE") {
      diag("ICE restart aborted — no active session", { reason });
      return;
    }

    const pc = this.viewerPeerConnection;
    if (!pc || pc.connectionState === "closed") {
      diag("ICE restart aborted — peer unavailable", { reason });
      return;
    }

    this.iceRestartInFlight = true;
    try {
      if (!signalBus.signalingConnected) {
        diag("ICE restart: signaling down, reconnect requested", { reason });
        await this.connectSignaling({ force: true, reason: "ice_restart" });
      }
      if (!signalBus.signalingConnected) {
        diag("ICE restart aborted — signaling still unavailable");
        this.scheduleIceRestart("signaling_unavailable", 2500);
        return;
      }

      const livePc = this.viewerPeerConnection;
      if (!livePc || livePc !== pc || livePc.signalingState === "closed") {
        diag("ICE restart aborted — peer changed/closed", { reason });
        return;
      }

      if (livePc.signalingState !== "stable") {
        diag("ICE restart deferred — signalingState not stable", {
          signalingState: livePc.signalingState
        });
        this.scheduleIceRestart("deferred_non_stable", 1200);
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
      await this.waitForIceGathering(livePc, 3000);

      const finalSdp = livePc.localDescription?.sdp ?? offer.sdp ?? "";
      const finalType = livePc.localDescription?.type ?? offer.type;
      const restartOffer = { type: finalType, sdp: finalSdp };

      this.sendViewerOfferPayload(String(session.id), restartOffer);
      this.startViewerOfferRetry(String(session.id), restartOffer);
      this.screenFrameError = "Reconnexion reseau en cours (ICE restart)...";
      diag("ICE restart offer sent", {
        sessionId: session.id,
        candidateCount: finalSdp.split("\n").filter((l) => l.startsWith("a=candidate")).length
      });
    } catch (error) {
      diag("ICE restart failed", String(error));
      this.scheduleIceRestart("retry_after_error", 2500);
    } finally {
      this.iceRestartInFlight = false;
    }
  };

  // ── ICE servers ─────────────────────────────────────────────────────────
  refreshViewerIceServers = async () => {
    try {
      const servers = await invoke<Array<{ urls: string[] | string; username?: string; credential?: string }>>(
        "get_ice_servers_cmd"
      );

      if (!Array.isArray(servers) || servers.length === 0) {
        this.viewerIceServers = resolveIceServers();
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
      this.viewerIceServers = normalized;

      if (this.viewerIceServers.length === 0) {
        this.viewerIceServers = resolveIceServers();
      }
    } catch {
      this.viewerIceServers = resolveIceServers();
    }
  };

  // ── Offer retry / gathering ─────────────────────────────────────────────
  stopViewerOfferRetry = () => {
    if (this.viewerOfferRetryTimer) {
      clearInterval(this.viewerOfferRetryTimer);
      this.viewerOfferRetryTimer = null;
    }
  };

  waitForIceGathering = (pc: RTCPeerConnection, timeoutMs: number): Promise<void> => {
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
  };

  sendViewerOfferPayload = (
    sessionId: string,
    offer: Pick<RTCSessionDescriptionInit, "type" | "sdp">
  ) => {
    const offerMessage: SignalMessage = {
      type: "OFFER",
      to: "agent",
      sessionId,
      payload: {
        type: offer.type,
        sdp: offer.sdp
      }
    };

    signalBus.client.send(offerMessage, "viewer");
    signalBus.logSignal("out", { ...offerMessage, from: "viewer" });
  };

  startViewerOfferRetry = (
    sessionId: string,
    offer: Pick<RTCSessionDescriptionInit, "type" | "sdp">
  ) => {
    this.stopViewerOfferRetry();
    this.viewerOfferRetryCount = 0;

    this.viewerOfferRetryTimer = setInterval(() => {
      if (this.viewerAnswerReceived || this.viewerPeerConnection?.connectionState === "connected") {
        this.stopViewerOfferRetry();
        return;
      }

      if (!signalBus.client.isConnected()) {
        return;
      }

      if (this.viewerOfferRetryCount >= MAX_VIEWER_OFFER_RETRIES) {
        this.stopViewerOfferRetry();
        if (!this.viewerAnswerReceived) {
          this.screenFrameError = "Aucune reponse SDP recue. Le viewer a cesse de renvoyer l'offre.";
        }
        return;
      }

      this.viewerOfferRetryCount += 1;

      try {
        this.sendViewerOfferPayload(sessionId, offer);
      } catch {
        // ignore transient signaling send issues
      }
    }, 1000);
  };

  // ── Controls auto-hide ──────────────────────────────────────────────────
  stopViewerControlsAutoHide = () => {
    if (this.viewerControlsTimer) {
      clearTimeout(this.viewerControlsTimer);
      this.viewerControlsTimer = null;
    }
  };

  revealViewerControls = () => {
    this.viewerControlsVisible = true;

    if (this.viewerConnectionState !== "connected") {
      this.stopViewerControlsAutoHide();
      return;
    }

    this.stopViewerControlsAutoHide();
    this.viewerControlsTimer = setTimeout(() => {
      this.viewerControlsVisible = false;
    }, 3000);
  };

  // ── Video metadata / jitter buffer ──────────────────────────────────────
  syncViewerVideoMetadata = (videoEl: HTMLVideoElement) => {
    if (videoEl.videoWidth > 0 && videoEl.videoHeight > 0) {
      this.viewerRemoteWidth = videoEl.videoWidth;
      this.viewerRemoteHeight = videoEl.videoHeight;
    }
  };

  viewerPlayoutDelayHint = (): number => {
    return this.viewerPlaybackProfile === "quality" ? 0.12 : 0.0;
  };

  applyViewerJitterBufferProfile = (pc: RTCPeerConnection | null) => {
    if (!pc) {
      return;
    }

    const playoutDelay = this.viewerPlayoutDelayHint();
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
  };

  // ── Playback profile / presets ──────────────────────────────────────────
  stopViewerAutoUpgradeTimer = () => {
    if (this.viewerProfileAutoUpgradeTimer) {
      clearTimeout(this.viewerProfileAutoUpgradeTimer);
      this.viewerProfileAutoUpgradeTimer = null;
    }
  };

  sendViewerPlaybackProfile = (
    profile: "responsive" | "quality",
    options?: { manualOverride?: boolean }
  ) => {
    this.viewerPlaybackProfile = profile;
    if (options?.manualOverride) {
      this.viewerProfileManualOverride = true;
    }

    this.applyViewerJitterBufferProfile(this.viewerPeerConnection);

    if (!streamProfileSignalEnabled) {
      return;
    }

    const current = this.getSession();
    if (!signalBus.signalingConnected || !current?.id) {
      return;
    }

    const bitrateBpsByTier: Record<"poor" | "medium" | "good", number> = {
      poor: 1_500_000,
      medium: 4_000_000,
      good: 8_000_000
    };

    const bitrateBps =
      this.viewerBitrateTier === "auto" ? undefined : bitrateBpsByTier[this.viewerBitrateTier];
    const fpsTier = this.viewerFpsTier === "auto" ? undefined : this.viewerFpsTier;

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
      signalBus.client.send(profileMessage, "viewer");
      signalBus.logSignal("out", { ...profileMessage, from: "viewer" });
    } catch {
      // Ignore transient signaling send issues.
    }
  };

  maybeAutoUpgradeViewerProfile = () => {
    const isEligible =
      signalBus.signalingConnected &&
      this.viewerConnectionState === "connected" &&
      this.viewerPlaybackProfile === "responsive" &&
      !this.viewerProfileManualOverride &&
      (this.viewerStreamMbps ?? 0) >= VIEWER_AUTO_UPGRADE_MIN_MBPS &&
      (this.viewerStreamFps ?? 0) >= VIEWER_AUTO_UPGRADE_MIN_FPS;

    if (!isEligible) {
      this.stopViewerAutoUpgradeTimer();
      return;
    }

    if (this.viewerProfileAutoUpgradeTimer) {
      return;
    }

    this.viewerProfileAutoUpgradeTimer = setTimeout(() => {
      this.viewerProfileAutoUpgradeTimer = null;

      const stillEligible =
        signalBus.signalingConnected &&
        this.viewerConnectionState === "connected" &&
        this.viewerPlaybackProfile === "responsive" &&
        !this.viewerProfileManualOverride &&
        (this.viewerStreamMbps ?? 0) >= VIEWER_AUTO_UPGRADE_MIN_MBPS &&
        (this.viewerStreamFps ?? 0) >= VIEWER_AUTO_UPGRADE_MIN_FPS;

      if (!stillEligible) {
        return;
      }

      this.sendViewerPlaybackProfile("quality");
    }, VIEWER_AUTO_UPGRADE_DELAY_MS);
  };

  toggleViewerPlaybackProfile = () => {
    this.stopViewerAutoUpgradeTimer();
    const nextProfile = this.viewerPlaybackProfile === "quality" ? "responsive" : "quality";
    this.viewerPreset = "custom";
    this.sendViewerPlaybackProfile(nextProfile, { manualOverride: true });
  };

  applyViewerStreamTuning = () => {
    this.stopViewerAutoUpgradeTimer();
    this.viewerPreset = "custom";
    this.sendViewerPlaybackProfile(this.viewerPlaybackProfile, { manualOverride: true });
  };

  applyViewerPreset = (preset: "low-latency" | "balanced" | "quality") => {
    this.viewerPreset = preset;

    if (preset === "low-latency") {
      this.viewerPlaybackProfile = "responsive";
      this.viewerFpsTier = "active";
      this.viewerBitrateTier = "medium";
    } else if (preset === "balanced") {
      this.viewerPlaybackProfile = "responsive";
      this.viewerFpsTier = "normal";
      this.viewerBitrateTier = "medium";
    } else {
      this.viewerPlaybackProfile = "quality";
      this.viewerFpsTier = "active";
      this.viewerBitrateTier = "good";
    }

    this.stopViewerAutoUpgradeTimer();
    this.sendViewerPlaybackProfile(this.viewerPlaybackProfile, { manualOverride: true });
  };

  // ── DataChannel "input" wiring ──────────────────────────────────────────
  configureViewerDataChannel = (channel: RTCDataChannel) => {
    this.viewerControlChannel = channel;
    this.viewerDataChannelOpen = channel.readyState === "open";

    channel.onopen = () => {
      this.viewerControlChannel = channel;
      this.viewerDataChannelOpen = true;
      this.screenFrameError = null;
      this.onControlChannelOpen();
    };

    channel.onclose = () => {
      if (this.viewerControlChannel === channel) {
        this.viewerDataChannelOpen = false;
        this.viewerKeyboardCaptured = false;
      }
    };

    channel.onerror = () => {
      if (this.viewerControlChannel === channel) {
        this.viewerDataChannelOpen = false;
        this.viewerKeyboardCaptured = false;
      }
    };

    channel.onmessage = (event: MessageEvent<string | ArrayBuffer>) => {
      if (typeof event.data !== "string") return;
      try {
        const payload = JSON.parse(event.data) as Record<string, unknown>;
        const msgType = typeof payload.type === "string" ? payload.type : "?";
        console.log(`[ai] ◀ DataChannel inbound type="${msgType}", len=${event.data.length}`);
        switch (msgType) {
          case "AI_ACTION_RESULT":
            this.handleAiActionResult(payload);
            break;
          case "screenshot_response":
          case "screenshot_response_error":
          case "screenshot_chunk_start":
          case "screenshot_chunk":
          case "screenshot_chunk_end":
            this.handleScreenshotResponse(payload);
            break;
          default:
            break;
        }
      } catch {
        // Pas un JSON — ignore silencieusement.
      }
    };
  };

  // ── DataChannel "privacy" wiring ────────────────────────────────────────
  // The viewer is the only side allowed to flip the blur. We keep the
  // last known intent (default: blur ON) and replay it on channel open
  // so a momentary signaling glitch can't leave the agent un-blurred.
  configurePrivacyDataChannel = (channel: RTCDataChannel) => {
    this.viewerPrivacyChannel = channel;
    this.viewerPrivacyChannelOpen = channel.readyState === "open";

    channel.onopen = () => {
      this.viewerPrivacyChannel = channel;
      this.viewerPrivacyChannelOpen = true;
      // Replay current intent so the agent and viewer agree from the
      // first frame — even if the technician had toggled OFF before
      // the channel finished opening.
      this.sendPrivacyBlurState(this.viewerPrivacyBlurEnabled);
    };

    channel.onclose = () => {
      if (this.viewerPrivacyChannel === channel) {
        this.viewerPrivacyChannelOpen = false;
      }
    };

    channel.onerror = () => {
      if (this.viewerPrivacyChannel === channel) {
        this.viewerPrivacyChannelOpen = false;
      }
    };
  };

  /** Push the current blur intent to the agent. Returns true if sent. */
  sendPrivacyBlurState = (enabled: boolean): boolean => {
    const channel = this.viewerPrivacyChannel;
    if (!channel || channel.readyState !== "open") {
      return false;
    }
    try {
      channel.send(JSON.stringify({ action: "set_blur", enabled }));
      return true;
    } catch {
      return false;
    }
  };

  /** Toggle (or explicitly set) the password blur. Updates UI state too. */
  setPrivacyBlurEnabled = (enabled: boolean): void => {
    this.viewerPrivacyBlurEnabled = enabled;
    this.sendPrivacyBlurState(enabled);
  };

  // ── Input gates / send ──────────────────────────────────────────────────
  canSendViewerInput = (): boolean => {
    const current = this.getSession();
    return (
      this.getSelectedFeature() === "screen" &&
      current?.status === "ACTIVE" &&
      current.allowRemoteInput !== false &&
      this.viewerDataChannelOpen &&
      !!this.viewerControlChannel
    );
  };

  canSendViewerKeyboardInput = (): boolean => {
    return this.canSendViewerInput() && this.viewerKeyboardCaptured;
  };

  sendViewerInput = (event: RemoteInputEvent): boolean => {
    if (!this.canSendViewerInput() || !this.viewerControlChannel) {
      return false;
    }

    try {
      this.viewerControlChannel.send(JSON.stringify(event));
      return true;
    } catch {
      this.viewerDataChannelOpen = false;
      this.viewerKeyboardCaptured = false;
      return false;
    }
  };

  // ── Input handlers ──────────────────────────────────────────────────────
  handleViewerVideoFocus = () => {
    this.viewerKeyboardCaptured = true;
    this.revealViewerControls();
  };

  handleViewerVideoBlur = () => {
    this.viewerKeyboardCaptured = false;
  };

  getViewerPointerPosition = (event: MouseEvent) => {
    const videoEl = this.viewerVideoEl;
    if (!videoEl) {
      return null;
    }

    const rect = videoEl.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) {
      return null;
    }

    const scaleX = this.viewerRemoteWidth / rect.width;
    const scaleY = this.viewerRemoteHeight / rect.height;
    const x = Math.min(Math.max(Math.round((event.clientX - rect.left) * scaleX), 0), this.viewerRemoteWidth - 1);
    const y = Math.min(Math.max(Math.round((event.clientY - rect.top) * scaleY), 0), this.viewerRemoteHeight - 1);

    return { x, y };
  };

  handleViewerMouseMove = (event: MouseEvent) => {
    this.revealViewerControls();

    if (!this.canSendViewerInput()) {
      return;
    }

    const position = this.getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    const now = performance.now();
    if (now - this.lastViewerMouseMoveSentAt < VIEWER_MOUSE_MOVE_MIN_INTERVAL_MS) {
      return;
    }

    if (this.lastViewerPointerSent && this.lastViewerPointerSent.x === position.x && this.lastViewerPointerSent.y === position.y) {
      return;
    }

    this.lastViewerMouseMoveSentAt = now;
    this.lastViewerPointerSent = position;

    void this.sendViewerInput({
      type: "mouse-move",
      x: position.x,
      y: position.y
    });
  };

  handleViewerMouseDown = (event: MouseEvent) => {
    this.revealViewerControls();

    if (!this.canSendViewerInput()) {
      return;
    }

    event.preventDefault();
    this.viewerVideoEl?.focus();

    const position = this.getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    void this.sendViewerInput({
      type: "mouse-down",
      button: event.button,
      x: position.x,
      y: position.y
    });
  };

  handleViewerMouseUp = (event: MouseEvent) => {
    this.revealViewerControls();

    if (!this.canSendViewerInput()) {
      return;
    }

    event.preventDefault();

    const position = this.getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    void this.sendViewerInput({
      type: "mouse-up",
      button: event.button,
      x: position.x,
      y: position.y
    });
  };

  handleViewerDoubleClick = (event: MouseEvent) => {
    this.revealViewerControls();

    if (!this.canSendViewerInput()) {
      return;
    }

    event.preventDefault();

    const position = this.getViewerPointerPosition(event);
    if (!position) {
      return;
    }

    void this.sendViewerInput({
      type: "dblclick",
      button: event.button,
      x: position.x,
      y: position.y
    });
  };

  handleViewerWheel = (event: WheelEvent) => {
    this.revealViewerControls();

    if (!this.canSendViewerInput()) {
      return;
    }

    const now = performance.now();
    if (now - this.lastViewerWheelSentAt < VIEWER_WHEEL_MIN_INTERVAL_MS) {
      event.preventDefault();
      return;
    }
    this.lastViewerWheelSentAt = now;

    event.preventDefault();
    void this.sendViewerInput({
      type: "wheel",
      deltaY: event.deltaY
    });
  };

  handleViewerDocumentKeyDown = (event: KeyboardEvent) => {
    if (!this.canSendViewerKeyboardInput() || isEditableTarget(event.target)) {
      return;
    }

    event.preventDefault();
    void this.sendViewerInput({
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
  };

  handleViewerDocumentKeyUp = (event: KeyboardEvent) => {
    if (!this.canSendViewerKeyboardInput() || isEditableTarget(event.target)) {
      return;
    }

    event.preventDefault();
    void this.sendViewerInput({
      type: "key-up",
      key: event.key,
      code: event.code
    });
  };

  // ── Fullscreen / expand ─────────────────────────────────────────────────
  syncViewerFullscreenState = () => {
    this.viewerFullscreenActive = !!this.viewerShellEl && document.fullscreenElement === this.viewerShellEl;
  };

  toggleViewerFullscreen = async () => {
    if (!this.viewerShellEl) {
      return;
    }

    if (document.fullscreenElement === this.viewerShellEl) {
      await document.exitFullscreen();
      return;
    }

    await this.viewerShellEl.requestFullscreen();
  };

  enterViewerFullscreen = async () => {
    if (!this.viewerShellEl || document.fullscreenElement === this.viewerShellEl) return;
    try { await this.viewerShellEl.requestFullscreen(); } catch { /* user gesture / API absente */ }
  };

  exitViewerFullscreen = async () => {
    if (document.fullscreenElement) {
      try { await document.exitFullscreen(); } catch { /* déjà sorti */ }
    }
  };

  toggleViewerExpanded = () => {
    this.viewerExpanded = !this.viewerExpanded;
  };

  // ── Peer connection lifecycle ───────────────────────────────────────────
  ensureViewerPeerConnection = (sessionId: string): RTCPeerConnection => {
    if (this.viewerPeerConnection) {
      return this.viewerPeerConnection;
    }

    // Logge l'IP physique de l'agent (best-effort) avant d'ouvrir la PC.
    // C'est purement informatif pour le debug — la sélection ICE est
    // toujours pilotée par les candidats annoncés des deux côtés.
    const session = this.getSession();
    if (session?.agentMachineId) {
      void technicianApi
        .getAgentNetwork(session.agentMachineId)
        .then((net) => console.info(`🛰 Agent ${session.agentMachineId} localIp=${net?.localIp || "?"}`))
        .catch((e) => console.warn(`🛰 getAgentNetwork failed for ${session.agentMachineId}:`, e));
    }

    diag("creating new RTCPeerConnection with iceServers", this.viewerIceServers);
    const pc = new RTCPeerConnection({
      iceServers: this.viewerIceServers,
      iceTransportPolicy: "all"
    });

    const inputChannel = pc.createDataChannel("input", { ordered: true });
    this.configureViewerDataChannel(inputChannel);

    const fileChannelInstance = pc.createDataChannel("file", { ordered: true });
    this.configureFileDataChannel(fileChannelInstance);

    // Privacy control channel — JSON-only, low traffic, ordered.
    // Carries `{ action: "set_blur", enabled: bool }` to toggle the
    // agent-side password blur. See PrivacyControl.svelte for the UI.
    const privacyChannel = pc.createDataChannel("privacy", { ordered: true });
    this.configurePrivacyDataChannel(privacyChannel);

    pc.addTransceiver("video", { direction: "recvonly" });

    this.viewerConnectionState = pc.connectionState;

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
      this.viewerRemoteStream = stream;
      this.screenFrameError = null;
      this.applyViewerJitterBufferProfile(pc);

      event.track.onmute = () => diag("track.onmute (no media flowing)", { id: event.track.id });
      event.track.onunmute = () => diag("track.onunmute (media resumed)", { id: event.track.id });
      event.track.onended = () => diag("track.onended (track terminated)", { id: event.track.id });
    };

    pc.onconnectionstatechange = () => {
      this.viewerConnectionState = pc.connectionState;
      diag("pc.connectionState =", pc.connectionState);
      if (pc.connectionState === "connected") {
        this.viewerHadConnectedOnce = true;
        this.revealViewerControls();
        this.screenFrameError = null;
        signalBus.signalingError = null;
        signalBus.stopReconnect();
        this.stopIceConvergenceWatchdog();
        this.applyViewerJitterBufferProfile(pc);
        this.maybeAutoUpgradeViewerProfile();
        this.startInboundStatsLogger(pc);
        this.stopIceRestartTimer();
        void this.diagnoseLanConnectivity(pc);
      } else if (pc.connectionState === "failed") {
        this.screenFrameError = "La connexion WebRTC a echoue.";
        this.stopInboundStatsLogger();
        this.stopIceConvergenceWatchdog();
        this.scheduleIceRestart("pc_connection_failed", ICE_RESTART_ON_FAILED_DELAY_MS);
      } else if (pc.connectionState === "closed" || pc.connectionState === "disconnected") {
        this.stopInboundStatsLogger();
        this.stopIceConvergenceWatchdog();
        if (pc.connectionState === "disconnected") {
          this.scheduleIceRestart("pc_connection_disconnected", ICE_RESTART_ON_DISCONNECTED_DELAY_MS);
        }
      }
    };

    pc.oniceconnectionstatechange = () => {
      diag("pc.iceConnectionState =", pc.iceConnectionState);
      if (pc.iceConnectionState === "connected" || pc.iceConnectionState === "completed") {
        this.stopIceRestartTimer();
        return;
      }
      if (pc.iceConnectionState === "failed") {
        this.scheduleIceRestart("ice_failed", ICE_RESTART_ON_FAILED_DELAY_MS);
        return;
      }
      if (pc.iceConnectionState === "disconnected") {
        this.scheduleIceRestart("ice_disconnected", ICE_RESTART_ON_DISCONNECTED_DELAY_MS);
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
      this.configureViewerDataChannel(event.channel);
    };

    pc.onicecandidate = (event) => {
      if (!event.candidate) {
        diag("ICE viewer: gathering complete (null candidate)");
        return;
      }

      const candStr = event.candidate.candidate;
      const cType = event.candidate.type ?? parseCandidateType(candStr);
      const cAddr = event.candidate.address
        ?? event.candidate.relatedAddress
        ?? parseCandidateAddress(candStr);
      const cPort = event.candidate.port ?? parseCandidatePort(candStr);

      // Filtre VPN / VirtualBox / VMware / APIPA / IPv6 link-local — sans ça,
      // un candidat host inutilisable peut être sélectionné par ICE et la
      // session ne converge pas.
      if (!isValidCandidate(event.candidate)) {
        console.warn(`🚫 [ICE] viewer SKIP  type=${cType} addr=${cAddr}:${cPort} (interface bloquée)`);
        return;
      }

      const iceMessage: SignalMessage = {
        type: "ICE",
        to: "agent",
        sessionId,
        payload: {
          candidate: candStr,
          sdpMid: event.candidate.sdpMid,
          sdpMLineIndex: event.candidate.sdpMLineIndex
        }
      };

      if (!signalBus.signalingConnected) {
        signalBus.bufferedLocalIceCandidates.push(iceMessage);
        console.info(
          `🧊 [ICE] viewer (buffered, signaling closed)  type=${cType} addr=${cAddr}:${cPort}`
        );
        return;
      }

      console.info(`🧊 [ICE] viewer → agent  type=${cType} addr=${cAddr}:${cPort}`);

      try {
        signalBus.client.send(iceMessage, "viewer");
        signalBus.logSignal("out", { ...iceMessage, from: "viewer" });
      } catch (err) {
        diag("ICE send to agent FAILED — buffering", String(err));
        signalBus.bufferedLocalIceCandidates.push(iceMessage);
      }
    };

    this.viewerPeerConnection = pc;
    return pc;
  };

  sendViewerOffer = async (sessionId: string) => {
    const pc = this.ensureViewerPeerConnection(sessionId);
    const offer = await pc.createOffer({ offerToReceiveVideo: true });
    await pc.setLocalDescription(offer);

    await this.waitForIceGathering(pc, 4000);

    const finalSdp = pc.localDescription?.sdp ?? offer.sdp ?? "";
    const finalType = pc.localDescription?.type ?? offer.type;
    const finalOffer = { type: finalType, sdp: finalSdp };

    const offerH264 = finalSdp.split("\n").filter((l) => /H264|h264/i.test(l));
    const candidateLines = finalSdp.split("\n").filter((l) => l.startsWith("a=candidate"));
    const relayCount = candidateLines.filter((l) => / typ relay/.test(l)).length;
    diag("OFFER created — H264 lines", offerH264);
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
    this.viewerAnswerReceived = false;
    this.screenFrameError = null;

    this.sendViewerOfferPayload(sessionId, finalOffer);
    this.startViewerOfferRetry(sessionId, finalOffer);
  };

  resetViewerPeerConnection = () => {
    diag("resetViewerPeerConnection CALLED");
    console.trace("[DIAG] resetViewerPeerConnection stack");
    signalBus.bufferedLocalIceCandidates = [];
    this.stopInboundStatsLogger();
    this.stopIceConvergenceWatchdog();
    this.stopIceRestartTimer();
    this.iceRestartInFlight = false;
    this.stopViewerOfferRetry();
    this.stopViewerControlsAutoHide();
    this.viewerAnswerReceived = false;
    this.viewerHadConnectedOnce = false;
    this.viewerSignalProcessing = Promise.resolve();
    this.pendingViewerIceCandidates = [];
    this.viewerOfferRetryCount = 0;
    this.viewerDataChannelOpen = false;
    this.viewerKeyboardCaptured = false;
    this.lastViewerMouseMoveSentAt = 0;
    this.lastViewerWheelSentAt = 0;
    this.lastViewerPointerSent = null;
    this.viewerConnectionState = "idle";
    this.viewerControlsVisible = true;
    this.viewerRemoteWidth = 1920;
    this.viewerRemoteHeight = 1080;
    this.viewerStreamMbps = null;
    this.viewerStreamFps = null;
    this.viewerLocalFps = null;
    this.viewerLocalMbps = null;
    this.viewerLocalRttMs = null;
    this.viewerLocalLossPct = null;
    this.viewerLocalJitterMs = null;
    this.viewerLocalResolution = null;
    this.viewerLocalFramesDropped = null;
    this.viewerFpsTier = "auto";
    this.viewerBitrateTier = "auto";
    this.viewerPreset = "balanced";
    this.stopViewerAutoUpgradeTimer();
    this.viewerProfileManualOverride = false;
    this.viewerPlaybackProfile = "responsive";

    try {
      this.viewerControlChannel?.close();
    } catch {
      // ignore close errors
    } finally {
      this.viewerControlChannel = null;
    }

    this.resetFileChannel();

    try {
      this.viewerPeerConnection?.close();
    } catch {
      // ignore close errors
    } finally {
      this.viewerPeerConnection = null;
    }

    if (this.viewerVideoEl) {
      try {
        this.viewerVideoEl.srcObject = null;
      } catch {
        // ignore
      }
    }

    try {
      this.viewerRemoteStream?.getTracks().forEach((track) => track.stop());
    } catch {
      // ignore
    }
    this.viewerRemoteStream = null;

    this.screenFrameError = null;
    this.viewerCandidatePairType = null;
    this.viewerLocalCandidateAddress = null;
    this.viewerRemoteCandidateAddress = null;
    this.viewerTransportSummary = null;
  };

  // ── Inbound signal dispatch ─────────────────────────────────────────────
  handleIncomingViewerSignal = async (message: SignalMessage): Promise<void> => {
    if (message.type === "STREAM_STATS") {
      const payload = message.payload as Record<string, unknown> | null;
      this.viewerStreamMbps = Number(payload?.mbps ?? 0);
      this.viewerStreamFps = Number(payload?.fps ?? 0);
      this.maybeAutoUpgradeViewerProfile();
      return;
    }

    if (message.type === "ERROR") {
      const payload = message.payload as Record<string, unknown> | null;
      const reason =
        (typeof payload?.error === "string" && payload.error) ||
        (typeof payload?.message === "string" && payload.message) ||
        "Erreur signaling recue depuis l'agent.";
      this.screenFrameError = reason;
      return;
    }

    if (message.type === "ANSWER") {
      const payload = message.payload as { type?: string; sdp?: string } | null;
      if (!payload?.sdp || !payload?.type) {
        diag("ANSWER ignored — empty payload", payload);
        return;
      }

      const pc = this.viewerPeerConnection;
      if (!pc) {
        diag("ANSWER ignored — no viewerPeerConnection");
        return;
      }

      this.viewerAnswerReceived = true;
      this.stopViewerOfferRetry();
      this.screenFrameError = null;

      const h264Lines = payload.sdp.split("\n").filter((l) => /H264|h264/i.test(l));
      diag("ANSWER received — H264 lines in SDP", h264Lines);

      try {
        await pc.setRemoteDescription({
          type: payload.type as RTCSdpType,
          sdp: payload.sdp
        });
        diag("setRemoteDescription OK", { signalingState: pc.signalingState });
        this.stopIceRestartTimer();
        this.iceRestartInFlight = false;
      } catch (err) {
        diag("setRemoteDescription FAILED", String(err));
        this.screenFrameError = `setRemoteDescription failed: ${String(err)}`;
        return;
      }

      this.sendViewerPlaybackProfile(this.viewerPlaybackProfile);
      this.maybeAutoUpgradeViewerProfile();

      if (this.pendingViewerIceCandidates.length > 0) {
        const queued = this.pendingViewerIceCandidates;
        this.pendingViewerIceCandidates = [];
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

      if (!payload?.candidate || !this.viewerPeerConnection) {
        return;
      }

      const candidateInit: RTCIceCandidateInit = {
        candidate: payload.candidate,
        sdpMid: payload.sdpMid ?? null,
        sdpMLineIndex: payload.sdpMLineIndex ?? null
      };

      const candStr = candidateInit.candidate ?? "";
      const typMatch = candStr.match(/typ (\w+)/);
      const candType = typMatch ? typMatch[1] : "?";
      diag("ICE from agent", { type: candType, candidate: candStr.slice(0, 80) });

      const pc = this.viewerPeerConnection;
      if (!pc.remoteDescription) {
        this.pendingViewerIceCandidates.push(candidateInit);
        diag("ICE queued (no remoteDescription yet)", { queueLen: this.pendingViewerIceCandidates.length });
        return;
      }

      try {
        await pc.addIceCandidate(candidateInit);
      } catch (error) {
        diag("addIceCandidate FAILED", String(error));
      }
    }
  };
}

export const viewerPeer = new ViewerPeer();
