<script lang="ts">
  import { aiPipeline } from "$lib/managers/ai-pipeline.svelte";

  // Overlay non-bloquant affiché par-dessus le flux vidéo quand l'IA pilote
  // la machine distante. Sert deux objectifs :
  //   1. Signaler visuellement le takeover (bordure pulsante + bandeau).
  //   2. Donner un bouton STOP toujours visible — pas besoin de chercher dans
  //      le chat side panel.
  let visible = $derived(aiPipeline.aiBusy);
</script>

{#if visible}
  <div class="rd-ai-takeover" role="status" aria-live="polite">
    <div class="rd-ai-takeover__frame" aria-hidden="true"></div>
    <div class="rd-ai-takeover__banner">
      <span class="rd-ai-takeover__pulse" aria-hidden="true"></span>
      <div class="rd-ai-takeover__text">
        <strong>
          IA en cours d'action
          {#if aiPipeline.agenticActive}
            <span class="rd-ai-takeover__iter">
              · tour {aiPipeline.agenticIteration + 1}/5
            </span>
          {/if}
        </strong>
        <span class="rd-ai-takeover__sub">
          {aiPipeline.aiLastRationale ?? "Analyse et exécution du plan…"}
        </span>
      </div>
      <button
        class="rd-ai-takeover__stop"
        type="button"
        onclick={() => aiPipeline.sendAiCancel()}
        title="Arrêter l'IA (Ctrl+Shift+X)">
        <span class="rd-ai-takeover__stop-icon" aria-hidden="true">■</span>
        <span>Stop</span>
      </button>
    </div>
  </div>
{/if}

<style>
  .rd-ai-takeover {
    position: absolute;
    inset: 0;
    pointer-events: none;
    z-index: 30;
  }
  /* Bordure animée pour signaler "IA pilote" sans bloquer la vue. */
  .rd-ai-takeover__frame {
    position: absolute;
    inset: 0;
    border: 3px solid #f59e0b;
    border-radius: 8px;
    box-shadow: inset 0 0 24px rgba(245, 158, 11, 0.35);
    animation: rd-ai-pulse 1.6s ease-in-out infinite;
  }
  @keyframes rd-ai-pulse {
    0%, 100% { opacity: 0.55; box-shadow: inset 0 0 24px rgba(245, 158, 11, 0.35); }
    50%      { opacity: 1;    box-shadow: inset 0 0 36px rgba(245, 158, 11, 0.55); }
  }
  .rd-ai-takeover__banner {
    position: absolute;
    top: 14px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 14px;
    background: rgba(10, 15, 21, 0.92);
    border: 1px solid #f59e0b;
    border-radius: 999px;
    padding: 8px 8px 8px 16px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    pointer-events: auto;
    max-width: 90%;
  }
  .rd-ai-takeover__pulse {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: #f59e0b;
    box-shadow: 0 0 12px #f59e0b;
    animation: rd-ai-dot 1s ease-in-out infinite;
    flex-shrink: 0;
  }
  @keyframes rd-ai-dot {
    0%, 100% { transform: scale(1);   opacity: 1; }
    50%      { transform: scale(0.6); opacity: 0.5; }
  }
  .rd-ai-takeover__text {
    display: flex;
    flex-direction: column;
    line-height: 1.25;
    min-width: 0;
  }
  .rd-ai-takeover__text strong {
    color: #fcd34d;
    font-size: 13px;
    font-weight: 600;
  }
  .rd-ai-takeover__iter {
    color: #5eead4;
    font-size: 12px;
    font-weight: 500;
    margin-left: 4px;
    font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  }
  .rd-ai-takeover__sub {
    color: #cbd5e1;
    font-size: 12px;
    max-width: 420px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rd-ai-takeover__stop {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: #dc2626;
    color: #fff;
    border: none;
    padding: 7px 14px;
    border-radius: 999px;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
    transition: background 0.15s, transform 0.1s;
  }
  .rd-ai-takeover__stop:hover { background: #ef4444; }
  .rd-ai-takeover__stop:active { transform: scale(0.96); }
  .rd-ai-takeover__stop-icon {
    font-size: 10px;
    line-height: 1;
  }
</style>
