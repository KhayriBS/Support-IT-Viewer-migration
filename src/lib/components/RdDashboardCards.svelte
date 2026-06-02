<script lang="ts">
  import { dashboardData } from "$lib/managers/dashboard-data.svelte";
  import { agentManager } from "$lib/managers/agent-manager.svelte";

  export type DashCard = "me" | "machines" | "users" | "history";

  interface Props {
    onPick: (card: DashCard) => void;
  }

  let { onPick }: Props = $props();

  const stats = $derived(dashboardData.stats);
</script>

<section class="cards">
  {#if dashboardData.error}
    <p class="cards__error">Erreur stats : {dashboardData.error}</p>
  {/if}

  <div class="cards__grid">
    <button class="card card--me" type="button" onclick={() => onPick("me")}>
      <div class="card__head"><span class="card__icon">💻</span><h3>Ma machine</h3></div>
      <div class="card__body">
        <p class="card__hostname">{agentManager.localMachineId || "—"}</p>
        {#if agentManager.localConnectionCode}
          <p class="card__metric">Code : <strong>{agentManager.localConnectionCode}</strong></p>
        {/if}
      </div>
      <span class="card__cta">Voir les métriques →</span>
    </button>

    <button class="card card--machines" type="button" onclick={() => onPick("machines")}>
      <div class="card__head"><span class="card__icon">🖥</span><h3>Machines</h3></div>
      <div class="card__body">
        <p class="card__big">{stats.onlineMachines}<span>/{stats.totalMachines} en ligne</span></p>
        {#if stats.unassignedMachines > 0}
          <p class="card__metric warn">{stats.unassignedMachines} non attribuée{stats.unassignedMachines > 1 ? "s" : ""}</p>
        {/if}
      </div>
      <span class="card__cta">Superviser →</span>
    </button>

    <button class="card card--users" type="button" onclick={() => onPick("users")}>
      <div class="card__head"><span class="card__icon">👤</span><h3>Utilisateurs</h3></div>
      <div class="card__body">
        <p class="card__big">{stats.totalUsers}<span>compte{stats.totalUsers > 1 ? "s" : ""}</span></p>
      </div>
      <span class="card__cta">Gérer →</span>
    </button>

    <button class="card card--history" type="button" onclick={() => onPick("history")}>
      <div class="card__head"><span class="card__icon">⏱</span><h3>Historiques</h3></div>
      <div class="card__body">
        <p class="card__big">{stats.activeSessions}<span>session{stats.activeSessions > 1 ? "s" : ""} active{stats.activeSessions > 1 ? "s" : ""}</span></p>
      </div>
      <span class="card__cta">Voir l'historique →</span>
    </button>
  </div>
</section>

<style>
  .cards { padding: 8px 0 16px; }
  .cards__error { color: #ff8484; font-size: 12px; margin: 0 0 12px; }
  .cards__grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 18px;
  }
  .card {
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    text-align: left;
    padding: 22px 22px 18px;
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 14px;
    color: inherit;
    font-family: inherit;
    cursor: pointer;
    transition: transform 120ms ease, background 120ms ease, border-color 120ms ease;
    min-height: 170px;
  }
  .card:hover {
    transform: translateY(-2px);
    background: rgba(255, 255, 255, 0.06);
    border-color: rgba(75, 158, 255, 0.35);
  }
  .card__head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 14px;
  }
  .card__head h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    opacity: 0.85;
  }
  .card__icon {
    width: 30px; height: 30px;
    display: inline-flex; align-items: center; justify-content: center;
    border-radius: 8px;
    background: rgba(75, 158, 255, 0.15);
    font-size: 16px;
  }
  .card__body { flex: 1; }
  .card__hostname {
    margin: 0;
    font-family: "JetBrains Mono", "Cascadia Code", ui-monospace, monospace;
    font-size: 13px;
    opacity: 0.85;
    word-break: break-all;
  }
  .card__metric { margin: 8px 0 0; font-size: 13px; opacity: 0.85; }
  .card__metric.warn { color: #ffb800; }
  .card__big {
    margin: 0;
    font-size: 30px;
    font-weight: 700;
    line-height: 1.1;
  }
  .card__big span {
    font-size: 13px;
    font-weight: 400;
    opacity: 0.7;
    margin-left: 8px;
  }
  .card__cta {
    margin-top: 16px;
    font-size: 12px;
    color: #4b9eff;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
</style>
