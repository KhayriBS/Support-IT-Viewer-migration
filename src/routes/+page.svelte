<script lang="ts">
  // Router racine : redirige vers la bonne vue selon le rôle reçu de l'agent.
  // Le travail (sync lifecycle + role) est déjà fait par +layout.svelte ;
  // on se contente d'afficher un écran transitoire le temps que le role tombe.
  import { agentManager } from "$lib/managers/agent-manager.svelte";
</script>

<svelte:head>
  <title>Lumière IT</title>
</svelte:head>

<main class="rd-bootstrap">
  <div class="bootstrap-card">
    <div class="spinner" aria-hidden="true"></div>
    <p class="bootstrap-title">Connexion à Lumière IT…</p>
    {#if agentManager.localMachineId}
      <p class="bootstrap-sub">Machine : <code>{agentManager.localMachineId}</code></p>
    {/if}
    {#if agentManager.agentLifecycleError}
      <p class="bootstrap-error">Erreur : {agentManager.agentLifecycleError}</p>
    {/if}
  </div>
</main>

<style>
  .rd-bootstrap {
    min-height: 100vh;
    display: grid;
    place-items: center;
    background: #0b1220;
    color: #e6ebf5;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  .bootstrap-card {
    text-align: center;
    padding: 32px 40px;
    border-radius: 12px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    min-width: 280px;
  }
  .spinner {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: 3px solid rgba(255, 255, 255, 0.18);
    border-top-color: #4b9eff;
    margin: 0 auto 16px;
    animation: spin 0.9s linear infinite;
  }
  .bootstrap-title { margin: 0; font-size: 15px; opacity: 0.9; }
  .bootstrap-sub { margin: 8px 0 0; font-size: 12px; opacity: 0.65; }
  .bootstrap-error { margin: 12px 0 0; font-size: 12px; color: #ff8484; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
