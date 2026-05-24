import { SignalingClient } from "$lib/api";
import type { SignalMessage } from "$lib/api";
import { formatSignalPayload } from "$lib/utils/signal";

export interface SignalLogEntry {
  timestamp: string;
  direction: "in" | "out";
  type: string;
  from: string;
  to: string;
  payload: string;
}

const uiDebugEnabled =
  import.meta.env.DEV &&
  String((import.meta as unknown as { env?: Record<string, unknown> }).env?.VITE_UI_DEBUG ?? "") === "1";

class SignalBus {
  client = new SignalingClient();

  signalingConnected = $state(false);
  signalingError = $state<string | null>(null);
  signalLogs = $state<SignalLogEntry[]>([]);

  manualDisconnect = false;
  remoteEnded = false;
  reconnectAttempts = 0;
  connectInFlight = false;
  bufferedLocalIceCandidates: SignalMessage[] = [];

  detachMessageListener: (() => void) | null = null;
  detachCloseListener: (() => void) | null = null;
  detachErrorListener: (() => void) | null = null;
  reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Predicate consulted before each reconnect attempt. Returns true if the
   * caller still wants to reconnect (e.g. session still ACTIVE). Set once by
   * the orchestrator during init.
   */
  shouldReconnect: () => boolean = () => false;

  /** Action fired when the reconnect timer elapses. */
  doReconnect: () => void = () => {};

  logSignal = (direction: "in" | "out", msg: SignalMessage) => {
    if (!uiDebugEnabled) return;
    if (msg.type === "FILE_DATA" || msg.type === "STREAM_STATS") return;

    const next: SignalLogEntry = {
      timestamp: new Date().toLocaleTimeString(),
      direction,
      type: msg.type,
      from: msg.from ?? "",
      to: msg.to,
      payload: formatSignalPayload(msg.type, msg.payload)
    };

    this.signalLogs = [next, ...this.signalLogs].slice(0, 16);
  };

  clearListeners = () => {
    this.detachMessageListener?.();
    this.detachCloseListener?.();
    this.detachErrorListener?.();
    this.detachMessageListener = null;
    this.detachCloseListener = null;
    this.detachErrorListener = null;
  };

  stopReconnect = () => {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  };

  scheduleReconnect = () => {
    this.stopReconnect();
    if (!this.shouldReconnect()) {
      return;
    }
    this.reconnectAttempts += 1;
    const delayMs = Math.min(1000 * 2 ** (this.reconnectAttempts - 1), 10000);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.doReconnect();
    }, delayMs);
  };
}

export const signalBus = new SignalBus();
