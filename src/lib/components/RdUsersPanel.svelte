<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { technicianApi } from "$lib/api";
  import type { AppUser } from "$lib/api";

  let users = $state<AppUser[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let search = $state("");
  let roleFilter = $state<"all" | "USER" | "ADMIN">("all");
  let lastRefresh = $state("");
  let refreshTimer: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    loading = true;
    try {
      users = (await technicianApi.listUsers()) ?? [];
      error = null;
      lastRefresh = new Date().toLocaleTimeString();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    return users.filter((u) => {
      if (roleFilter !== "all" && u.role !== roleFilter) return false;
      if (!q) return true;
      return (
        u.username.toLowerCase().includes(q)
        || (u.email ?? "").toLowerCase().includes(q)
        || (u.fullName ?? "").toLowerCase().includes(q)
      );
    });
  });

  onMount(() => {
    void refresh();
    refreshTimer = setInterval(refresh, 15_000);
  });

  onDestroy(() => {
    if (refreshTimer) clearInterval(refreshTimer);
  });
</script>

<section class="rd-panel users">
  <header class="users__head">
    <h2 class="rd-panel__title"><span class="rd-icon">👤</span> Utilisateurs</h2>
    <div class="users__meta">
      <span>{filtered.length} / {users.length}</span>
      {#if lastRefresh}<span class="sub">· MAJ {lastRefresh}</span>{/if}
      <button class="users__reload" type="button" onclick={refresh} disabled={loading}>
        {loading ? "…" : "Rafraîchir"}
      </button>
    </div>
  </header>

  <div class="users__filters">
    <input
      class="users__search"
      type="search"
      placeholder="Rechercher (nom, email, username)…"
      bind:value={search} />
    <select class="users__select" bind:value={roleFilter}>
      <option value="all">Tous les rôles</option>
      <option value="ADMIN">Techniciens (ADMIN)</option>
      <option value="USER">Utilisateurs (USER)</option>
    </select>
  </div>

  {#if error}
    <p class="users__error">{error}</p>
  {/if}

  {#if filtered.length === 0 && !loading}
    <p class="users__empty">Aucun utilisateur trouvé.</p>
  {:else}
    <div class="users__table">
      <div class="users__row users__row--head">
        <span>Username</span>
        <span>Nom complet</span>
        <span>Email</span>
        <span>Rôle</span>
        <span>Statut</span>
      </div>
      {#each filtered as u (u.id)}
        <div class="users__row">
          <span class="cell-name"><code>{u.username}</code></span>
          <span>{u.fullName ?? "—"}</span>
          <span class="cell-email">{u.email ?? "—"}</span>
          <span>
            <span class="badge {u.role === 'ADMIN' ? 'admin' : 'user'}">
              {u.role === 'ADMIN' ? 'TECHNICIEN' : 'USER'}
            </span>
          </span>
          <span>
            <span class="badge {u.enabled === false ? 'off' : 'ok'}">
              {u.enabled === false ? 'Désactivé' : 'Actif'}
            </span>
          </span>
        </div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .users {
    padding: 20px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 12px;
  }
  .users__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }
  .users__meta {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    opacity: 0.75;
  }
  .sub { opacity: 0.6; }
  .users__reload {
    background: rgba(75, 158, 255, 0.16);
    color: #4b9eff;
    border: 1px solid rgba(75, 158, 255, 0.3);
    border-radius: 6px;
    padding: 5px 12px;
    font-size: 12px;
    cursor: pointer;
  }
  .users__reload:disabled { opacity: 0.4; cursor: not-allowed; }
  .users__filters {
    display: grid;
    grid-template-columns: 1fr 220px;
    gap: 10px;
    margin-bottom: 12px;
  }
  .users__search, .users__select {
    background: rgba(255, 255, 255, 0.06);
    color: inherit;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 13px;
  }
  .users__error { color: #ff8484; font-size: 13px; margin: 8px 0; }
  .users__empty { font-size: 13px; opacity: 0.7; }
  .users__table {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 13px;
  }
  .users__row {
    display: grid;
    grid-template-columns: 1.3fr 1.3fr 2fr 1fr 1fr;
    gap: 12px;
    padding: 10px 12px;
    align-items: center;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.02);
  }
  .users__row--head {
    background: transparent;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 11px;
    opacity: 0.65;
  }
  .users__row code {
    font-family: "JetBrains Mono", "Cascadia Code", ui-monospace, monospace;
    font-size: 12px;
  }
  .cell-name { font-weight: 600; }
  .cell-email { opacity: 0.8; }
  .badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
  }
  .badge.admin { background: rgba(168, 85, 247, 0.2); color: #c084fc; }
  .badge.user  { background: rgba(75, 158, 255, 0.18); color: #4b9eff; }
  .badge.ok    { background: rgba(46, 196, 121, 0.18); color: #2ec479; }
  .badge.off   { background: rgba(255, 132, 132, 0.18); color: #ff8484; }
</style>
