<script lang="ts">
  import { viewerPeer } from "$lib/managers/viewer-peer.svelte";

  interface Props {
    /** Optional compact mode — hides the warning banner when shown
     *  inside a small toolbar (the badge + button still render). */
    compact?: boolean;
  }

  let { compact = false }: Props = $props();

  // Reactive view of the manager's state.
  let blurEnabled = $derived(viewerPeer.viewerPrivacyBlurEnabled);
  let channelOpen = $derived(viewerPeer.viewerPrivacyChannelOpen);

  function toggleBlur() {
    if (!channelOpen) {
      // Optimistically flip locally; the manager will replay the intent
      // when the channel opens.
      viewerPeer.viewerPrivacyBlurEnabled = !viewerPeer.viewerPrivacyBlurEnabled;
      return;
    }
    viewerPeer.setPrivacyBlurEnabled(!blurEnabled);
  }
</script>

<div class="privacy-control">
  <button
    type="button"
    class="privacy-control__toggle"
    class:privacy-control__toggle--off={!blurEnabled}
    class:privacy-control__toggle--disconnected={!channelOpen}
    onclick={toggleBlur}
    disabled={!channelOpen}
    title={channelOpen
      ? blurEnabled
        ? "Désactiver le floutage des mots de passe"
        : "Activer le floutage des mots de passe"
      : "Canal privacy non connecté"}
    aria-pressed={blurEnabled}
  >
    <span class="ti ti-lock privacy-control__icon" aria-hidden="true"></span>
    <span class="privacy-control__label">Masquer mots de passe</span>
  </button>

  <span
    class="privacy-control__badge"
    class:privacy-control__badge--on={blurEnabled}
    class:privacy-control__badge--off={!blurEnabled}
  >
    {blurEnabled ? "Protection active" : "Désactivé"}
  </span>
</div>

{#if !blurEnabled && !compact}
  <div class="privacy-control__warning" role="alert">
    <span class="ti ti-alert-triangle" aria-hidden="true"></span>
    Attention&nbsp;: les mots de passe de l'utilisateur sont visibles.
  </div>
{/if}

<style>
  .privacy-control {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
  }

  .privacy-control__toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.4rem 0.75rem;
    border: 1px solid #2e7d32;
    border-radius: 8px;
    background: rgba(46, 125, 50, 0.12);
    color: #1b5e20;
    font-weight: 600;
    cursor: pointer;
    transition: background 120ms ease, border-color 120ms ease, color 120ms ease;
  }

  .privacy-control__toggle:hover:not(:disabled) {
    background: rgba(46, 125, 50, 0.22);
  }

  .privacy-control__toggle--off {
    border-color: #ef6c00;
    background: rgba(239, 108, 0, 0.12);
    color: #b35900;
  }

  .privacy-control__toggle--off:hover:not(:disabled) {
    background: rgba(239, 108, 0, 0.22);
  }

  .privacy-control__toggle--disconnected,
  .privacy-control__toggle:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .privacy-control__icon {
    font-size: 1.05rem;
    line-height: 1;
  }

  .privacy-control__label {
    font-size: 0.9rem;
    white-space: nowrap;
  }

  .privacy-control__badge {
    display: inline-flex;
    align-items: center;
    padding: 0.2rem 0.55rem;
    border-radius: 999px;
    font-size: 0.78rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
  }

  .privacy-control__badge--on {
    background: #2e7d32;
    color: #ffffff;
  }

  .privacy-control__badge--off {
    background: #ef6c00;
    color: #ffffff;
  }

  .privacy-control__warning {
    margin-top: 0.5rem;
    padding: 0.6rem 0.9rem;
    border-left: 4px solid #ef6c00;
    background: rgba(239, 108, 0, 0.1);
    color: #5d2a00;
    font-size: 0.88rem;
    border-radius: 6px;
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
</style>
