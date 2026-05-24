// Types partagés du gestionnaire de pair WebRTC côté technicien.

/**
 * Évènement d'entrée distant émis du viewer vers l'agent via le DataChannel
 * `input`. Tous les champs sont optionnels et leur présence dépend du `type` :
 *
 *  - `mousemove` / `mouse-move` : x, y
 *  - `mousedown` / `mouseup` / `click` / `dblclick` : x, y, button
 *  - `wheel` : x, y, deltaY
 *  - `keydown` / `keyup` : key, code, keyCode, modifiers
 *
 * Côté Rust, désérialisé par `input_handler.rs::handle_input`.
 */
export interface RemoteInputEvent {
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

/** Profil de lecture demandé par l'utilisateur (UI / preset). */
export type ViewerPlaybackProfile = "responsive" | "quality";

/** Cible de FPS — `auto` laisse l'adaptive controller décider. */
export type ViewerFpsTier = "auto" | "idle" | "normal" | "active";

/** Cible de bitrate — `auto` = pas de plafond manuel. */
export type ViewerBitrateTier = "auto" | "poor" | "medium" | "good";

/** Preset utilisateur global (combine FPS + bitrate + qualité). */
export type ViewerPreset = "custom" | "low-latency" | "balanced" | "quality";
