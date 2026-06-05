<script lang="ts">
  import { aiPipeline, type PendingShellApproval } from "$lib/managers/ai-pipeline.svelte";

  // On consomme le premier élément de la file. Tant qu'il en reste, le modal
  // reste ouvert avec le suivant. Le filtrage est fait par respondShellApproval()
  // qui retire l'entrée avant d'envoyer la décision sur le DataChannel.
  let current = $derived<PendingShellApproval | null>(
    aiPipeline.pendingShellApprovals[0] ?? null
  );

  let remainingSec = $state(60);
  let tickHandle: ReturnType<typeof setInterval> | null = null;

  // Recalcule le compte à rebours toutes les secondes basé sur receivedAt.
  // L'agent Rust timeout à 60s ; si on dépasse, on refuse automatiquement
  // côté UI pour cohérence (sinon l'utilisateur clique "Approuver" sur une
  // requête que l'agent a déjà déclinée).
  $effect(() => {
    if (current) {
      const refresh = () => {
        const elapsed = Math.floor((Date.now() - current!.receivedAt) / 1000);
        remainingSec = Math.max(0, 60 - elapsed);
        if (remainingSec === 0) {
          // Auto-deny par cohérence avec le timeout Rust côté agent.
          aiPipeline.respondShellApproval(current!.actionId, false);
        }
      };
      refresh();
      tickHandle = setInterval(refresh, 1000);
    } else if (tickHandle) {
      clearInterval(tickHandle);
      tickHandle = null;
    }
    return () => {
      if (tickHandle) {
        clearInterval(tickHandle);
        tickHandle = null;
      }
    };
  });
</script>

{#if current}
  <div class="rd-ai-shell-overlay" role="dialog" aria-modal="true" aria-labelledby="rd-ai-shell-title">
    <div class="rd-ai-shell-modal">
      <header class="rd-ai-shell-head">
        <span class="rd-ai-shell-icon" aria-hidden="true">🔐</span>
        <div>
          <h2 id="rd-ai-shell-title">L'IA demande l'exécution d'une commande shell</h2>
          <p class="rd-ai-shell-sub">
            Cette commande n'est pas dans la liste pré-approuvée. Vérifie-la avant d'autoriser.
          </p>
        </div>
      </header>

      <div class="rd-ai-shell-cmd-block">
        <div class="rd-ai-shell-meta">
          <span class="rd-ai-shell-pill">shell : {current.shell}</span>
          <span class="rd-ai-shell-pill rd-ai-shell-pill--timer" class:rd-ai-shell-pill--warn={remainingSec <= 15}>
            Expire dans {remainingSec}s
          </span>
        </div>
        <pre class="rd-ai-shell-cmd"><code>{current.cmd}</code></pre>
      </div>

      <div class="rd-ai-shell-warn">
        <strong>⚠️ À vérifier :</strong> chemins absolus suspects, suppression récursive,
        modification du registre, redémarrage forcé. En cas de doute, refuse.
      </div>

      <div class="rd-ai-shell-actions">
        <button
          class="rd-ai-shell-btn rd-ai-shell-btn--deny"
          type="button"
          onclick={() => aiPipeline.respondShellApproval(current!.actionId, false)}>
          Refuser
        </button>
        <button
          class="rd-ai-shell-btn rd-ai-shell-btn--allow"
          type="button"
          onclick={() => aiPipeline.respondShellApproval(current!.actionId, true)}>
          Autoriser l'exécution
        </button>
      </div>

      {#if aiPipeline.pendingShellApprovals.length > 1}
        <p class="rd-ai-shell-queue">
          {aiPipeline.pendingShellApprovals.length - 1} autre(s) commande(s) en attente après celle-ci.
        </p>
      {/if}
    </div>
  </div>
{/if}

<style>
  .rd-ai-shell-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.75);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1200;
    backdrop-filter: blur(3px);
  }
  .rd-ai-shell-modal {
    background: #11181f;
    border: 1px solid #f59e0b;
    border-radius: 14px;
    padding: 24px 26px;
    width: min(94vw, 620px);
    box-shadow: 0 24px 70px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(245, 158, 11, 0.2);
  }
  .rd-ai-shell-head {
    display: flex;
    align-items: flex-start;
    gap: 14px;
    margin-bottom: 18px;
  }
  .rd-ai-shell-icon {
    font-size: 28px;
    line-height: 1;
    flex-shrink: 0;
  }
  .rd-ai-shell-head h2 {
    margin: 0 0 4px 0;
    font-size: 17px;
    color: #fff;
  }
  .rd-ai-shell-sub {
    margin: 0;
    font-size: 13px;
    color: #cbd5e1;
  }
  .rd-ai-shell-cmd-block {
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 8px;
    padding: 12px;
    margin-bottom: 16px;
  }
  .rd-ai-shell-meta {
    display: flex;
    gap: 8px;
    margin-bottom: 10px;
    flex-wrap: wrap;
  }
  .rd-ai-shell-pill {
    background: #1f2a36;
    color: #cbd5e1;
    font-size: 11px;
    padding: 3px 8px;
    border-radius: 4px;
    font-weight: 500;
    font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
  }
  .rd-ai-shell-pill--timer { background: #134e4a; color: #5eead4; }
  .rd-ai-shell-pill--warn { background: #7c2d12; color: #fdba74; }
  .rd-ai-shell-cmd {
    margin: 0;
    padding: 0;
    background: transparent;
    color: #f8fafc;
    font-family: ui-monospace, "SFMono-Regular", Consolas, monospace;
    font-size: 13px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 240px;
    overflow-y: auto;
  }
  .rd-ai-shell-cmd code { background: transparent; color: inherit; }
  .rd-ai-shell-warn {
    background: rgba(245, 158, 11, 0.08);
    border: 1px solid rgba(245, 158, 11, 0.25);
    color: #fcd34d;
    border-radius: 8px;
    padding: 10px 12px;
    font-size: 12.5px;
    margin-bottom: 18px;
    line-height: 1.5;
  }
  .rd-ai-shell-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
  }
  .rd-ai-shell-btn {
    padding: 10px 18px;
    border-radius: 8px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    border: 1px solid transparent;
    transition: background 0.15s, border-color 0.15s;
  }
  .rd-ai-shell-btn--deny {
    background: transparent;
    border-color: #1f2a36;
    color: #cbd5e1;
  }
  .rd-ai-shell-btn--deny:hover {
    background: rgba(239, 68, 68, 0.12);
    border-color: rgba(239, 68, 68, 0.4);
    color: #fca5a5;
  }
  .rd-ai-shell-btn--allow {
    background: #f59e0b;
    color: #0d1117;
  }
  .rd-ai-shell-btn--allow:hover { background: #fbbf24; }
  .rd-ai-shell-queue {
    margin: 14px 0 0 0;
    font-size: 12px;
    color: #94a3b8;
    text-align: right;
  }
</style>
