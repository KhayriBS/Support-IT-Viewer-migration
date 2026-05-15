<script lang="ts">
  import type { SessionHistoryEntry } from "$lib/api/types";
  import type { RdSessionTypeFilter, RdSessionStatusFilter } from "$lib/types/ui";
  import { rdFormatTime, rdFormatDuration } from "$lib/utils/format";

  interface Props {
    entries: SessionHistoryEntry[];
    error: string | null;
    loading: boolean;
    search: string;
    typeFilter: RdSessionTypeFilter;
    statusFilter: RdSessionStatusFilter;
  }

  let {
    entries,
    error,
    loading,
    search = $bindable(),
    typeFilter = $bindable(),
    statusFilter = $bindable()
  }: Props = $props();
</script>

<section class="rd-panel rd-history">
  <header class="rd-history__head">
    <h2 class="rd-panel__title"><span class="rd-icon">⏱</span> Historique des sessions</h2>
    <span class="rd-history__count">{entries.length} session{entries.length > 1 ? "s" : ""}</span>
  </header>
  <input
    class="rd-history__search"
    type="search"
    placeholder="Rechercher par code machine..."
    bind:value={search} />
  <div class="rd-history__filters">
    <select class="rd-select" bind:value={typeFilter}>
      <option value="all">Tous les types</option>
      <option value="incoming">Entrantes</option>
      <option value="outgoing">Sortantes</option>
    </select>
    <select class="rd-select" bind:value={statusFilter}>
      <option value="all">Tous les statuts</option>
      <option value="active">En cours</option>
      <option value="ended">Terminées</option>
    </select>
  </div>
  <div class="rd-history__list">
    {#if error}
      <p class="rd-empty">Erreur API: {error}</p>
    {:else if loading && entries.length === 0}
      <p class="rd-empty">Chargement…</p>
    {:else if entries.length === 0}
      <p class="rd-empty">Aucune session pour les filtres actuels.</p>
    {:else}
      {#each entries as session (session.id)}
        {@const isActive = session.status !== "TERMINATED"}
        <article class="rd-session">
          <div class="rd-session__top">
            <strong class="rd-session__code">{session.peerLabel}</strong>
            {#if isActive}
              <span class="rd-pill rd-pill--live">En cours</span>
            {:else}
              <span class="rd-pill rd-pill--done">Terminée</span>
            {/if}
          </div>
          <p class="rd-session__type">
            Connexion {session.direction === "incoming" ? "entrante" : "sortante"}
          </p>
          <p class="rd-session__meta">
            Début: {rdFormatTime(session.startedAt)}
            {#if !isActive && session.durationMs}
              &nbsp;&nbsp; Durée: {rdFormatDuration(session.durationMs)}
            {/if}
          </p>
        </article>
      {/each}
    {/if}
  </div>
</section>

<style>
  .rd-session {
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 10px;
    padding: 12px 14px;
  }
  .rd-session__top {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 6px;
  }
  .rd-session__code {
    color: #38bdf8;
    font-family: "Consolas", monospace;
    font-size: 14px;
  }
  .rd-session__type {
    margin: 0 0 4px 0;
    font-size: 13px;
    color: #cbd5e1;
  }
  .rd-session__meta {
    margin: 0;
    font-size: 12px;
    color: #64748b;
  }
  .rd-pill {
    font-size: 11px;
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid transparent;
  }
  .rd-pill--done {
    background: rgba(148, 163, 184, 0.12);
    color: #cbd5e1;
    border-color: rgba(148, 163, 184, 0.2);
  }
  .rd-pill--live {
    background: rgba(74, 222, 128, 0.15);
    color: #4ade80;
    border-color: rgba(74, 222, 128, 0.3);
  }
</style>
