<script lang="ts">
  interface Props {
    visible: boolean;
    streamPresent: boolean;
    fps: number | null;
    mbps: number | null;
    rttMs: number | null;
    lossPct: number | null;
    jitterMs: number | null;
    resolution: string | null;
  }

  let {
    visible = $bindable(),
    streamPresent,
    fps,
    mbps,
    rttMs,
    lossPct,
    jitterMs,
    resolution
  }: Props = $props();
</script>

{#if visible && streamPresent}
  <div class="rd-viewer__stats-bar">
    <div class="rd-stats__cell" title="Images par seconde décodées">
      <span class="rd-stats__icon">🎞</span>
      <span class="rd-stats__num">{fps !== null ? fps.toFixed(0) : "--"}</span>
      <span class="rd-stats__unit">FPS</span>
    </div>
    <div class="rd-stats__cell" title="Débit vidéo entrant">
      <span class="rd-stats__icon">📶</span>
      <span class="rd-stats__num">{mbps !== null ? mbps.toFixed(2) : "--"}</span>
      <span class="rd-stats__unit">Mb/s</span>
    </div>
    <div
      class="rd-stats__cell"
      class:rd-stats__cell--warn={rttMs !== null && rttMs > 150}
      class:rd-stats__cell--bad={rttMs !== null && rttMs > 300}
      title="Latence aller-retour (ICE candidate-pair nominée)">
      <span class="rd-stats__icon">⏱</span>
      <span class="rd-stats__num">{rttMs !== null ? rttMs.toFixed(0) : "--"}</span>
      <span class="rd-stats__unit">ms</span>
    </div>
    <div
      class="rd-stats__cell"
      class:rd-stats__cell--warn={lossPct !== null && lossPct > 1}
      class:rd-stats__cell--bad={lossPct !== null && lossPct > 5}
      title="Paquets perdus sur la dernière seconde">
      <span class="rd-stats__icon">📉</span>
      <span class="rd-stats__num">{lossPct !== null ? lossPct.toFixed(1) : "--"}</span>
      <span class="rd-stats__unit">%</span>
    </div>
    <div class="rd-stats__cell" title="Gigue (jitter)">
      <span class="rd-stats__icon">📊</span>
      <span class="rd-stats__num">{jitterMs !== null ? jitterMs.toFixed(0) : "--"}</span>
      <span class="rd-stats__unit">ms</span>
    </div>
    {#if resolution}
      <div class="rd-stats__cell" title="Résolution de la trame reçue">
        <span class="rd-stats__icon">🖼</span>
        <span class="rd-stats__num">{resolution}</span>
      </div>
    {/if}
    <button
      class="rd-stats__close"
      type="button"
      onclick={() => { visible = false; }}
      title="Masquer la barre de stats">×</button>
  </div>
{:else if streamPresent}
  <button
    class="rd-viewer__stats-restore"
    type="button"
    onclick={() => { visible = true; }}
    title="Afficher les stats">📊</button>
{/if}

<style>
  .rd-viewer__stats-bar {
    position: absolute;
    top: 12px;
    left: 12px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    background: rgba(13, 17, 23, 0.7);
    border: 1px solid rgba(56, 189, 248, 0.2);
    border-radius: 999px;
    backdrop-filter: blur(10px) saturate(1.2);
    -webkit-backdrop-filter: blur(10px) saturate(1.2);
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.4);
    color: #e2e8f0;
    font-size: 12px;
    font-family: "Consolas", monospace;
    z-index: 11;
    flex-wrap: wrap;
    max-width: calc(100% - 24px);
  }
  .rd-stats__cell {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px 9px;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    transition: background 0.15s, border-color 0.15s, color 0.15s;
  }
  .rd-stats__cell--warn {
    background: rgba(250, 204, 21, 0.12);
    border-color: rgba(250, 204, 21, 0.4);
    color: #facc15;
  }
  .rd-stats__cell--bad {
    background: rgba(239, 68, 68, 0.15);
    border-color: rgba(239, 68, 68, 0.45);
    color: #fca5a5;
  }
  .rd-stats__icon { font-size: 13px; line-height: 1; opacity: 0.9; }
  .rd-stats__num {
    color: #fff;
    font-weight: 600;
    min-width: 1ch;
  }
  .rd-stats__cell--warn .rd-stats__num { color: #facc15; }
  .rd-stats__cell--bad .rd-stats__num { color: #fca5a5; }
  .rd-stats__unit { color: #94a3b8; font-size: 11px; }
  .rd-stats__close {
    background: transparent;
    border: 1px solid rgba(255, 255, 255, 0.12);
    color: #cbd5e1;
    width: 22px; height: 22px;
    border-radius: 50%;
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 0;
    margin-left: 2px;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }
  .rd-stats__close:hover {
    background: rgba(239, 68, 68, 0.15);
    color: #fff;
    border-color: rgba(239, 68, 68, 0.4);
  }

  .rd-viewer__stats-restore {
    position: absolute;
    top: 12px;
    left: 12px;
    width: 32px; height: 32px;
    border-radius: 50%;
    background: rgba(13, 17, 23, 0.7);
    border: 1px solid rgba(56, 189, 248, 0.25);
    color: #cbd5e1;
    cursor: pointer;
    font-size: 14px;
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
    z-index: 11;
    transition: background 0.15s, color 0.15s;
  }
  .rd-viewer__stats-restore:hover {
    background: rgba(56, 189, 248, 0.18);
    color: #fff;
  }
</style>
