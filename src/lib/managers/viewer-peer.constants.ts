// Constantes du gestionnaire de pair WebRTC côté technicien.
// Extraites de viewer-peer.svelte.ts pour pouvoir être unit-testées
// et réutilisées depuis d'autres modules (debug overlays, tests E2E).

/** Throttle pour les évènements `mousemove` envoyés sur le DataChannel — 90 Hz max. */
export const VIEWER_MOUSE_MOVE_MIN_INTERVAL_MS = 1000 / 90;

/** Throttle pour les évènements `wheel` — 60 Hz max. */
export const VIEWER_WHEEL_MIN_INTERVAL_MS = 1000 / 60;

/**
 * Délai d'attente avant de proposer un upgrade automatique de profil
 * (responsive → quality) lorsque la liaison est stable.
 */
export const VIEWER_AUTO_UPGRADE_DELAY_MS = 7000;

/** Débit minimum (Mbps) pour considérer un upgrade automatique. */
export const VIEWER_AUTO_UPGRADE_MIN_MBPS = 1.8;

/** FPS minimum pour considérer un upgrade automatique. */
export const VIEWER_AUTO_UPGRADE_MIN_FPS = 28;

/**
 * Nombre maximum de retries de l'offre SDP avant d'abandonner et d'afficher
 * une erreur à l'utilisateur. Combiné à l'interval de 1 s côté retry loop.
 */
export const MAX_VIEWER_OFFER_RETRIES = 60;

/**
 * Fenêtre pendant laquelle on attend la convergence ICE avant de tirer
 * l'alarme (peut indiquer un firewall qui bloque le TURN).
 */
export const ICE_CONVERGENCE_WINDOW_MS = 15000;

/** Délai avant ICE restart sur état `disconnected`. */
export const ICE_RESTART_ON_DISCONNECTED_DELAY_MS = 5000;

/** Délai avant ICE restart sur état `failed`. */
export const ICE_RESTART_ON_FAILED_DELAY_MS = 1200;

/**
 * Feature flag : émet-on les payloads `STREAM_PROFILE` via le canal de
 * signaling ? Désactivable en build pour les tests.
 */
export const streamProfileSignalEnabled =
  String(
    import.meta.env.VITE_ENABLE_STREAM_PROFILE_SIGNAL ?? "true",
  )
    .trim()
    .toLowerCase() !== "false";
