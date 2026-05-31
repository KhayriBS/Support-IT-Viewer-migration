<script lang="ts">
  // Vue restreinte USER : liste des machines attribuées au propriétaire de
  // cette machine. Lecture seule — pas de bouton "démarrer session". L'approbation
  // d'une demande entrante reste gérée par le modal du +layout.svelte.
  import { onDestroy, onMount } from "svelte";
  import { onAgentUpdate, technicianApi } from "$lib/api";
  import type { Agent } from "$lib/api";
  import { agentManager } from "$lib/managers/agent-manager.svelte";

  let machines = $state<Agent[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let lastRefresh = $state<string>("");
  let refreshTimer: ReturnType<typeof setInterval> | null = null;
  let unsubscribeRealtime: (() => void) | null = null;

  async function refresh() {
    loading = true;
    try {
      machines = (await technicianApi.getMyMachines()) ?? [];
      error = null;
      lastRefresh = new Date().toLocaleTimeString();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function statusBadgeClass(status: string | undefined) {
    if (status === "ONLINE") return "badge ok";
    if (status === "BUSY") return "badge busy";
    return "badge off";
  }

  onMount(() => {
    void refresh();
    refreshTimer = setInterval(refresh, 30_000);

    unsubscribeRealtime = onAgentUpdate((updated) => {
      // Patch en place si la machine est déjà dans la liste, sinon re-fetch
      // (le owner peut avoir reçu une nouvelle attribution).
      const idx = machines.findIndex((m) => m.id === updated.id);
      if (idx >= 0) {
        machines = [...machines.slice(0, idx), updated, ...machines.slice(idx + 1)];
        lastRefresh = new Date().toLocaleTimeString();
      } else if (updated.assignedUsername === agentManager.assignedUsername) {
        void refresh();
      }
    });
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
    unsubscribeRealtime?.();
  });
</script>

<svelte:head>
  <title>Lumière IT — Mes machines</title>
</svelte:head>

<main class="user-page">
  <header class="header">
    <h1>Mes machines</h1>
    <div class="meta">
      <span>Connecté en tant que <strong>{agentManager.assignedUsername ?? "—"}</strong></span>
      <span class="dot"></span>
      <span>Cette machine : <code>{agentManager.localMachineId}</code></span>
      {#if lastRefresh}
        <span class="dot"></span>
        <span class="refresh-label">Mis à jour à {lastRefresh}</span>
      {/if}
    </div>
  </header>

  {#if error}
    <p class="error">Erreur : {error}</p>
  {/if}

  {#if loading && machines.length === 0}
    <p class="info">Chargement…</p>
  {:else if machines.length === 0}
    <p class="info">Aucune machine ne vous est attribuée pour le moment.</p>
  {:else}
    <table class="machines">
      <thead>
        <tr>
          <th>Hostname</th>
          <th>Identifiant matériel</th>
          <th>OS</th>
          <th>Statut</th>
          <th>Dernier ping</th>
        </tr>
      </thead>
      <tbody>
        {#each machines as m (m.id)}
          <tr class:current={m.machineId === agentManager.localMachineId}>
            <td>{m.hostname ?? "—"}</td>
            <td><code>{m.machineId}</code></td>
            <td>{m.os ?? "—"}</td>
            <td><span class={statusBadgeClass(m.status)}>{m.status ?? "—"}</span></td>
            <td>{m.lastHeartbeat ?? "—"}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="hint">
      Lecture seule. Toute prise de contrôle à distance demande votre approbation
      via le pop-up qui s'affichera ici.
    </p>
  {/if}
</main>

<style>
  .user-page {
    min-height: 100vh;
    padding: 32px;
    background: #0b1220;
    color: #e6ebf5;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  .header { margin-bottom: 24px; }
  h1 { margin: 0 0 8px; font-size: 22px; }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    opacity: 0.8;
  }
  .dot {
    width: 4px; height: 4px; border-radius: 50%;
    background: rgba(255,255,255,0.3);
  }
  .machines {
    width: 100%;
    border-collapse: collapse;
    background: rgba(255, 255, 255, 0.03);
    border-radius: 10px;
    overflow: hidden;
  }
  .machines th, .machines td {
    text-align: left;
    padding: 12px 16px;
    font-size: 13px;
    border-bottom: 1px solid rgba(255,255,255,0.06);
  }
  .machines th {
    background: rgba(255,255,255,0.05);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    opacity: 0.7;
  }
  .machines tr.current { background: rgba(75, 158, 255, 0.06); }
  .machines code {
    font-family: "JetBrains Mono", "Cascadia Code", ui-monospace, monospace;
    font-size: 12px;
  }
  .badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
  }
  .badge.ok { background: rgba(46, 196, 121, 0.18); color: #2ec479; }
  .badge.busy { background: rgba(255, 184, 0, 0.18); color: #ffb800; }
  .badge.off { background: rgba(255, 132, 132, 0.18); color: #ff8484; }
  .info { font-size: 14px; opacity: 0.75; }
  .error { color: #ff8484; font-size: 13px; }
  .hint { margin-top: 16px; font-size: 12px; opacity: 0.6; }
</style>
