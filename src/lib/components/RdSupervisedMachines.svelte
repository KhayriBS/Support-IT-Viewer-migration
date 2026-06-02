<script lang="ts">
  import { technicianApi } from "$lib/api";
  import { dashboardData } from "$lib/managers/dashboard-data.svelte";

  let assigningId = $state<number | null>(null);
  let pickedUserByMachine = $state<Record<number, number | "">>({});
  let actionError = $state<string | null>(null);

  const machines = $derived(dashboardData.machines);
  const users = $derived(dashboardData.users);
  const lastRefresh = $derived(dashboardData.lastRefresh);
  const loading = $derived(dashboardData.loadingMachines);

  async function assign(machineId: number) {
    const userId = pickedUserByMachine[machineId];
    if (!userId) return;
    assigningId = machineId;
    actionError = null;
    try {
      await technicianApi.assignMachine(machineId, Number(userId));
      // STOMP pousse l'update sur /topic/agents → patch in-place automatique
      // via dashboard-data ; on déclenche un refresh stats juste pour synchro
      // au cas où le broadcast STOMP serait en retard.
      void dashboardData.refreshDashboard();
    } catch (e) {
      actionError = String(e);
    } finally {
      assigningId = null;
    }
  }

  function badge(status: string | undefined) {
    if (status === "ONLINE") return "ok";
    if (status === "BUSY") return "busy";
    return "off";
  }
</script>

<section class="rd-panel sup">
  <header class="sup__head">
    <h2 class="rd-panel__title"><span class="rd-icon">🖥</span> Machines supervisées</h2>
    <div class="sup__meta">
      <span>{machines.length} machine{machines.length > 1 ? "s" : ""}</span>
      {#if lastRefresh}<span class="sub">· MAJ {lastRefresh}</span>{/if}
      <button class="sup__reload" type="button" onclick={dashboardData.refreshDashboard} disabled={loading}>
        {loading ? "…" : "Rafraîchir"}
      </button>
    </div>
  </header>

  {#if actionError}
    <p class="sup__error">{actionError}</p>
  {/if}

  {#if machines.length === 0 && !loading}
    <p class="sup__empty">Aucune machine enregistrée.</p>
  {:else}
    <div class="sup__table">
      <div class="sup__row sup__row--head">
        <span>Hostname</span>
        <span>Machine ID</span>
        <span>OS</span>
        <span>Statut</span>
        <span>Propriétaire</span>
        <span>Code</span>
        <span class="sup__action-col">Action</span>
      </div>
      {#each machines as m (m.id)}
        <div class="sup__row">
          <span class="cell-name">{m.hostname ?? "—"}</span>
          <span class="cell-mid"><code>{m.machineId}</code></span>
          <span>{m.os ?? "—"}</span>
          <span><span class="badge {badge(m.status)}">{m.status ?? "—"}</span></span>
          <span class="cell-owner">{m.assignedUsername ?? "—"}</span>
          <span><code>{m.connectionCode ?? "—"}</code></span>
          <span class="sup__action">
            <select
              class="sup__select"
              bind:value={pickedUserByMachine[m.id]}
              disabled={assigningId === m.id}>
              <option value="">Attribuer à…</option>
              {#each users as u (u.id)}
                <option value={u.id}>{u.username} ({u.role})</option>
              {/each}
            </select>
            <button
              type="button"
              class="sup__assign-btn"
              disabled={!pickedUserByMachine[m.id] || assigningId === m.id}
              onclick={() => void assign(m.id)}>
              {assigningId === m.id ? "…" : "OK"}
            </button>
          </span>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .sup {
    margin: 24px 0;
    padding: 20px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 12px;
  }
  .sup__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 16px;
  }
  .sup__meta {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    opacity: 0.75;
  }
  .sub { opacity: 0.6; }
  .sup__reload {
    background: rgba(75, 158, 255, 0.16);
    color: #4b9eff;
    border: 1px solid rgba(75, 158, 255, 0.3);
    border-radius: 6px;
    padding: 5px 12px;
    font-size: 12px;
    cursor: pointer;
  }
  .sup__reload:disabled { opacity: 0.4; cursor: not-allowed; }
  .sup__error { color: #ff8484; font-size: 13px; margin: 8px 0; }
  .sup__empty { font-size: 13px; opacity: 0.7; }
  .sup__table {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 13px;
  }
  .sup__row {
    display: grid;
    grid-template-columns: 1.3fr 1.5fr 0.9fr 0.9fr 1fr 0.8fr 1.7fr;
    gap: 12px;
    padding: 10px 12px;
    align-items: center;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.02);
  }
  .sup__row--head {
    background: transparent;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    opacity: 0.65;
    padding-bottom: 4px;
  }
  .sup__row code {
    font-family: "JetBrains Mono", "Cascadia Code", ui-monospace, monospace;
    font-size: 12px;
    color: #b9c5d8;
  }
  .cell-name { font-weight: 600; }
  .cell-owner { opacity: 0.85; }
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
  .sup__action {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .sup__select {
    flex: 1;
    background: rgba(255, 255, 255, 0.06);
    color: inherit;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 6px;
    padding: 5px 8px;
    font-size: 12px;
  }
  .sup__assign-btn {
    background: rgba(75, 158, 255, 0.16);
    color: #4b9eff;
    border: 1px solid rgba(75, 158, 255, 0.3);
    border-radius: 6px;
    padding: 5px 10px;
    font-size: 12px;
    cursor: pointer;
  }
  .sup__assign-btn:disabled { opacity: 0.35; cursor: not-allowed; }
</style>
