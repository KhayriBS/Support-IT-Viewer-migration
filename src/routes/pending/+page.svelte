<script lang="ts">
  // Écran d'attente quand l'agent n'a encore aucun propriétaire assigné.
  // Deux mécanismes de bascule automatique :
  //  - STOMP /topic/agents (instantané) : si un event arrive sur NOTRE machineId
  //    avec un assignedUsername défini, on déclenche refreshRole().
  //  - Polling 30 s dans +layout.svelte (fallback si STOMP HS).
  import { onDestroy, onMount } from "svelte";
  import { onAgentUpdate } from "$lib/api";
  import { agentManager } from "$lib/managers/agent-manager.svelte";

  let unsubscribeRealtime: (() => void) | null = null;

  async function copyMachineId() {
    if (!agentManager.localMachineId) return;
    try {
      await navigator.clipboard.writeText(agentManager.localMachineId);
    } catch {
      /* ignore */
    }
  }

  async function refreshNow() {
    await agentManager.refreshRole();
  }

  onMount(() => {
    unsubscribeRealtime = onAgentUpdate((updated) => {
      if (updated.machineId === agentManager.localMachineId
          && updated.assignedUsername
          && updated.assignedUsername.trim() !== "") {
        void agentManager.refreshRole();
      }
    });
  });

  onDestroy(() => {
    unsubscribeRealtime?.();
  });
</script>

<svelte:head>
  <title>Lumière IT — En attente d'attribution</title>
</svelte:head>

<main class="pending-page">
  <section class="pending-card">
    <h1 class="title">Machine non attribuée</h1>
    <p class="subtitle">
      Cette machine attend qu'un technicien Lumière IT l'attribue à un utilisateur.
      Aucune action n'est requise — l'application se mettra à jour automatiquement.
    </p>

    <div class="info-row">
      <span class="label">Identifiant matériel</span>
      <code class="value">{agentManager.localMachineId || "…"}</code>
      <button class="copy-btn" type="button" onclick={copyMachineId}
              disabled={!agentManager.localMachineId}>Copier</button>
    </div>

    {#if agentManager.localConnectionCode}
      <div class="info-row">
        <span class="label">Code de connexion</span>
        <code class="value">{agentManager.localConnectionCode}</code>
      </div>
    {/if}

    <p class="hint">Communiquez l'identifiant matériel au technicien si nécessaire.</p>

    <button class="refresh-btn" type="button" onclick={refreshNow}>
      Vérifier maintenant
    </button>

    {#if agentManager.agentLifecycleError}
      <p class="error">Erreur : {agentManager.agentLifecycleError}</p>
    {/if}
  </section>
</main>

<style>
  .pending-page {
    min-height: 100vh;
    display: grid;
    place-items: center;
    background: #0b1220;
    color: #e6ebf5;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
    padding: 24px;
  }
  .pending-card {
    width: min(520px, 100%);
    padding: 32px;
    background: rgba(255, 255, 255, 0.05);
    border-radius: 14px;
    border: 1px solid rgba(255, 255, 255, 0.08);
  }
  .title { margin: 0 0 8px; font-size: 22px; }
  .subtitle { margin: 0 0 24px; font-size: 14px; line-height: 1.5; opacity: 0.8; }
  .info-row {
    display: grid;
    grid-template-columns: 160px 1fr auto;
    align-items: center;
    gap: 10px;
    margin: 12px 0;
  }
  .label { font-size: 12px; opacity: 0.7; text-transform: uppercase; letter-spacing: 0.05em; }
  .value {
    font-family: "JetBrains Mono", "Cascadia Code", ui-monospace, monospace;
    background: rgba(255, 255, 255, 0.06);
    padding: 6px 10px;
    border-radius: 6px;
    font-size: 13px;
    word-break: break-all;
  }
  .copy-btn, .refresh-btn {
    background: rgba(75, 158, 255, 0.16);
    color: #4b9eff;
    border: 1px solid rgba(75, 158, 255, 0.3);
    border-radius: 6px;
    padding: 6px 12px;
    font-size: 12px;
    cursor: pointer;
  }
  .copy-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .refresh-btn { margin-top: 24px; padding: 10px 18px; font-size: 13px; width: 100%; }
  .hint { font-size: 12px; opacity: 0.6; margin: 16px 0 0; }
  .error { color: #ff8484; font-size: 12px; margin: 16px 0 0; }
</style>
