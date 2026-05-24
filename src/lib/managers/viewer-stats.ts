// Pure helpers de calcul de statistiques de stream côté viewer.
//
// Extraits de viewer-peer.svelte.ts pour pouvoir être unit-testés sans monter
// un vrai RTCPeerConnection. Aucun état réactif Svelte ici — juste de la
// transformation de structures.
//
// Consommé par `ViewerPeer.startInboundStatsLogger` qui orchestre le poll
// périodique (setInterval) et la propagation vers les champs `$state`.

/**
 * Compteurs cumulés observés au tick précédent. Conservés en local par le
 * polling pour calculer un delta à chaque tick.
 */
export interface InboundStatsCounters {
  bytes: number;
  framesDecoded: number;
  packetsReceived: number;
  packetsLost: number;
  timestampMs: number;
}

/**
 * Le snapshot calculé à partir d'un delta entre deux ticks.
 * Toutes les valeurs sont déjà normalisées et prêtes à être affichées.
 */
export interface InboundStatsSample {
  /** Débit instantané en Mbps (delta_bytes * 8 / Δt / 1e6). */
  mbps: number;
  /** FPS instantané (frames décodées par seconde). */
  fps: number;
  /** Pourcentage de paquets perdus sur la fenêtre. */
  lossPct: number;
  /** Jitter en millisecondes (renvoyé par WebRTC en secondes). */
  jitterMs: number;
  /** Total cumulé de frames droppées par le décodeur. */
  framesDropped: number;
  /** Résolution rendue côté décodeur, sous la forme "WxH" ou null. */
  resolution: string | null;
}

/**
 * Forme minimale d'un `RTCInboundRtpStreamStats` filtré sur kind=video.
 * On extrait uniquement les champs qu'on consomme — TypeScript n'exporte
 * pas un type complet pour ces stats car les implémentations divergent.
 */
export interface InboundRtpVideoLike {
  bytesReceived?: number;
  framesDecoded?: number;
  framesDropped?: number;
  packetsReceived?: number;
  packetsLost?: number;
  jitter?: number;
  frameWidth?: number;
  frameHeight?: number;
}

/**
 * Calcule l'échantillon courant à partir d'une stat inbound-rtp video et
 * du tick précédent. Renvoie aussi les nouveaux compteurs (à conserver
 * pour le tick suivant).
 *
 * Pure function : pas d'effet de bord, deterministe.
 */
export function computeInboundSample(
  stat: InboundRtpVideoLike,
  prev: InboundStatsCounters,
  nowMs: number,
): { sample: InboundStatsSample; counters: InboundStatsCounters } {
  const elapsedSec = Math.max(0.001, (nowMs - prev.timestampMs) / 1000);

  const bytes = stat.bytesReceived ?? 0;
  const framesDecoded = stat.framesDecoded ?? 0;
  const packetsReceived = stat.packetsReceived ?? 0;
  const packetsLost = stat.packetsLost ?? 0;

  // Les compteurs WebRTC sont monotones croissants — un delta négatif
  // signifie qu'on a perdu une session ou un seek (reset à 0). Clamper
  // à 0 évite des valeurs absurdes affichées dans l'UI.
  const deltaBytes = Math.max(0, bytes - prev.bytes);
  const deltaFramesDecoded = Math.max(0, framesDecoded - prev.framesDecoded);
  const deltaPacketsReceived = Math.max(0, packetsReceived - prev.packetsReceived);
  const deltaPacketsLost = Math.max(0, packetsLost - prev.packetsLost);

  const mbps = (deltaBytes * 8) / 1_000_000 / elapsedSec;
  const fps = deltaFramesDecoded / elapsedSec;
  const totalPackets = deltaPacketsReceived + deltaPacketsLost;
  const lossPct = totalPackets > 0 ? (deltaPacketsLost / totalPackets) * 100 : 0;

  const resolution =
    stat.frameWidth && stat.frameHeight ? `${stat.frameWidth}×${stat.frameHeight}` : null;

  return {
    sample: {
      mbps,
      fps,
      lossPct,
      jitterMs: (stat.jitter ?? 0) * 1000,
      framesDropped: stat.framesDropped ?? 0,
      resolution,
    },
    counters: {
      bytes,
      framesDecoded,
      packetsReceived,
      packetsLost,
      timestampMs: nowMs,
    },
  };
}

/**
 * Forme minimale d'un candidate-pair WebRTC. On ne lit que les champs
 * utiles à la détection du chemin actif et du RTT.
 */
export interface CandidatePairLike {
  type: string;
  state?: string;
  nominated?: boolean;
  currentRoundTripTime?: number;
}

/**
 * Cherche le candidate-pair "nominated + succeeded" et retourne son RTT
 * en millisecondes. Renvoie `null` si aucune paire active n'est trouvée
 * (e.g. ICE en cours de convergence).
 */
export function extractActiveRttMs(pairs: Iterable<CandidatePairLike>): number | null {
  for (const p of pairs) {
    if (
      p.type === "candidate-pair" &&
      p.nominated &&
      p.state === "succeeded" &&
      typeof p.currentRoundTripTime === "number"
    ) {
      return p.currentRoundTripTime * 1000;
    }
  }
  return null;
}

/**
 * Crée un état initial de compteurs (tous à zéro sauf le timestamp).
 * À appeler au démarrage du logger pour éviter un premier delta absurde.
 */
export function initialCounters(nowMs: number): InboundStatsCounters {
  return {
    bytes: 0,
    framesDecoded: 0,
    packetsReceived: 0,
    packetsLost: 0,
    timestampMs: nowMs,
  };
}
