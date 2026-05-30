<script lang="ts">
  import { viewerPeer } from "$lib/managers/viewer-peer.svelte";
  import type { ViewerPreset } from "$lib/managers/viewer-peer.types";

  // Three canonical presets that map to combinations of
  // viewerPlaybackProfile + viewerFpsTier + viewerBitrateTier.
  // The mapping itself lives in viewer-peer::applyViewerPreset.
  type SelectablePreset = Exclude<ViewerPreset, "custom">;

  interface PresetOption {
    id: SelectablePreset;
    icon: string;
    label: string;
    hint: string;
  }

  const PRESETS: PresetOption[] = [
    {
      id: "low-latency",
      icon: "⚡",
      label: "Fluide",
      hint: "Latence minimale, qualité réduite — idéal pour la saisie"
    },
    {
      id: "balanced",
      icon: "⚖",
      label: "Équilibré",
      hint: "Compromis FPS / netteté — défaut recommandé"
    },
    {
      id: "quality",
      icon: "✨",
      label: "Qualité",
      hint: "Bitrate élevé, 60 fps — pour vidéo / images"
    }
  ];

  let current = $derived(viewerPeer.viewerPreset);

  function apply(preset: SelectablePreset) {
    if (preset === current) return;
    viewerPeer.applyViewerPreset(preset);
  }
</script>

<div
  class="rd-quality"
  role="group"
  aria-label="Profil de qualité du flux"
>
  {#each PRESETS as preset (preset.id)}
    <button
      type="button"
      class="rd-quality__seg"
      class:rd-quality__seg--active={current === preset.id}
      onclick={() => apply(preset.id)}
      title={preset.hint}
      aria-pressed={current === preset.id}
    >
      <span class="rd-quality__icon" aria-hidden="true">{preset.icon}</span>
      <span class="rd-quality__label">{preset.label}</span>
    </button>
  {/each}
  {#if current === "custom"}
    <span class="rd-quality__custom-badge" title="Réglages manuels actifs">
      Manuel
    </span>
  {/if}
</div>

<style>
  .rd-quality {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    padding: 3px;
    border-radius: 10px;
    background: rgba(15, 23, 42, 0.55);
    backdrop-filter: blur(6px);
    border: 1px solid rgba(148, 163, 184, 0.25);
  }

  .rd-quality__seg {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0.35rem 0.65rem;
    border: 0;
    background: transparent;
    color: #cbd5e1;
    border-radius: 7px;
    font-size: 0.82rem;
    font-weight: 500;
    cursor: pointer;
    transition: background 130ms ease, color 130ms ease;
  }

  .rd-quality__seg:hover:not(.rd-quality__seg--active) {
    background: rgba(148, 163, 184, 0.18);
    color: #f1f5f9;
  }

  .rd-quality__seg--active {
    background: #2563eb;
    color: #ffffff;
    box-shadow: 0 1px 4px rgba(37, 99, 235, 0.45);
  }

  .rd-quality__icon {
    font-size: 0.95rem;
    line-height: 1;
  }

  .rd-quality__label {
    white-space: nowrap;
  }

  .rd-quality__custom-badge {
    padding: 0.2rem 0.55rem;
    margin-left: 0.25rem;
    border-radius: 999px;
    background: #ef6c00;
    color: #ffffff;
    font-size: 0.7rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
</style>
