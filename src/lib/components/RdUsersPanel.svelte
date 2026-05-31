<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { technicianApi } from "$lib/api";
  import type { AppUser } from "$lib/api";

  type FormState = {
    id: number | null;
    username: string;
    email: string;
    fullName: string;
    phoneNumber: string;
    department: string;
    role: "USER" | "ADMIN";
    enabled: boolean;
    password: string;
  };

  function emptyForm(): FormState {
    return {
      id: null,
      username: "",
      email: "",
      fullName: "",
      phoneNumber: "",
      department: "",
      role: "USER",
      enabled: true,
      password: ""
    };
  }

  let users = $state<AppUser[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let search = $state("");
  let roleFilter = $state<"all" | "USER" | "ADMIN">("all");
  let lastRefresh = $state("");
  let refreshTimer: ReturnType<typeof setInterval> | null = null;

  // État du modal d'édition / création
  let formOpen = $state(false);
  let form = $state<FormState>(emptyForm());
  let saving = $state(false);
  let formError = $state<string | null>(null);

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

  function openCreate() {
    form = emptyForm();
    formError = null;
    formOpen = true;
  }

  function openEdit(u: AppUser) {
    form = {
      id: u.id,
      username: u.username,
      email: u.email ?? "",
      fullName: u.fullName ?? "",
      phoneNumber: u.phoneNumber ?? "",
      department: u.department ?? "",
      role: u.role,
      enabled: u.enabled !== false,
      password: ""
    };
    formError = null;
    formOpen = true;
  }

  function closeForm() {
    if (saving) return;
    formOpen = false;
    formError = null;
  }

  async function save() {
    if (!form.username.trim()) {
      formError = "Le username est obligatoire";
      return;
    }
    saving = true;
    formError = null;
    try {
      const payload: Partial<AppUser> & { password?: string } = {
        username: form.username.trim(),
        email: form.email.trim() || undefined,
        fullName: form.fullName.trim() || undefined,
        phoneNumber: form.phoneNumber.trim() || undefined,
        department: form.department.trim() || undefined,
        role: form.role,
        enabled: form.enabled
      };
      // Le password n'est envoyé que s'il est rempli. Pour un USER on le laisse
      // vide, pour un ADMIN on peut le set ou le laisser tel quel à l'edit.
      if (form.password.trim()) {
        payload.password = form.password;
      }

      if (form.id == null) {
        await technicianApi.createUser({ ...payload, username: payload.username! });
      } else {
        await technicianApi.updateUser(form.id, payload);
      }
      formOpen = false;
      await refresh();
    } catch (e) {
      formError = String(e);
    } finally {
      saving = false;
    }
  }

  async function remove(u: AppUser) {
    if (!confirm(`Supprimer l'utilisateur "${u.username}" ?\nToutes ses machines seront désaffectées.`)) {
      return;
    }
    try {
      await technicianApi.deleteUser(u.id);
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

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
      <button class="users__new" type="button" onclick={openCreate}>+ Nouveau</button>
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
        <span class="cell-actions">Actions</span>
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
          <span class="cell-actions">
            <button class="btn-edit" type="button" onclick={() => openEdit(u)}>Modifier</button>
            <button class="btn-del" type="button" onclick={() => void remove(u)}>Supprimer</button>
          </span>
        </div>
      {/each}
    </div>
  {/if}
</section>

{#if formOpen}
  <div
    class="modal-backdrop"
    role="button"
    tabindex="0"
    onclick={closeForm}
    onkeydown={(e) => { if (e.key === "Escape") closeForm(); }}>
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}>
      <h3 class="modal__title">{form.id == null ? "Nouvel utilisateur" : `Modifier "${form.username}"`}</h3>

      <div class="modal__grid">
        <label>
          <span>Username *</span>
          <input type="text" bind:value={form.username} placeholder="ex. jdupont" />
        </label>
        <label>
          <span>Rôle</span>
          <select bind:value={form.role}>
            <option value="USER">USER (utilisateur)</option>
            <option value="ADMIN">ADMIN (technicien)</option>
          </select>
        </label>
        <label>
          <span>Nom complet</span>
          <input type="text" bind:value={form.fullName} placeholder="Jean Dupont" />
        </label>
        <label>
          <span>Email</span>
          <input type="email" bind:value={form.email} placeholder="jean.dupont@lumiere.tn" />
        </label>
        <label>
          <span>Téléphone</span>
          <input type="text" bind:value={form.phoneNumber} placeholder="+216 …" />
        </label>
        <label>
          <span>Département</span>
          <input type="text" bind:value={form.department} placeholder="Direction technique" />
        </label>
        <label class="modal__checkbox">
          <input type="checkbox" bind:checked={form.enabled} />
          <span>Compte actif</span>
        </label>
        <label>
          <span>
            Mot de passe
            <em class="hint">(optionnel — USER n'en a pas besoin)</em>
          </span>
          <input type="password" bind:value={form.password}
                 placeholder={form.id == null ? "Laisser vide pour un USER" : "Laisser vide pour ne pas changer"} />
        </label>
      </div>

      {#if formError}
        <p class="modal__error">{formError}</p>
      {/if}

      <div class="modal__actions">
        <button type="button" class="btn-cancel" onclick={closeForm} disabled={saving}>Annuler</button>
        <button type="button" class="btn-save" onclick={() => void save()} disabled={saving}>
          {saving ? "Enregistrement…" : (form.id == null ? "Créer" : "Enregistrer")}
        </button>
      </div>
    </div>
  </div>
{/if}

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
    opacity: 0.85;
  }
  .sub { opacity: 0.6; }
  .users__reload, .users__new {
    color: #4b9eff;
    border: 1px solid rgba(75, 158, 255, 0.3);
    border-radius: 6px;
    padding: 5px 12px;
    font-size: 12px;
    cursor: pointer;
    background: rgba(75, 158, 255, 0.16);
  }
  .users__new { background: rgba(46, 196, 121, 0.18); color: #2ec479; border-color: rgba(46, 196, 121, 0.3); }
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
    grid-template-columns: 1.1fr 1.2fr 1.7fr 0.8fr 0.8fr 1.3fr;
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
  .cell-actions { display: flex; gap: 6px; justify-content: flex-end; }
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
  .btn-edit, .btn-del {
    background: rgba(255, 255, 255, 0.06);
    color: inherit;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 6px;
    padding: 4px 10px;
    font-size: 12px;
    cursor: pointer;
  }
  .btn-edit:hover { background: rgba(75, 158, 255, 0.18); border-color: rgba(75, 158, 255, 0.4); }
  .btn-del { color: #ff8484; border-color: rgba(255, 132, 132, 0.3); }
  .btn-del:hover { background: rgba(255, 132, 132, 0.18); }

  /* Modal */
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(8, 12, 22, 0.75);
    display: grid;
    place-items: center;
    z-index: 100;
    border: 0;
    cursor: default;
  }
  .modal {
    background: #131c2e;
    color: #e6ebf5;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 14px;
    padding: 24px;
    width: min(640px, 92vw);
    max-height: 90vh;
    overflow-y: auto;
    cursor: default;
  }
  .modal__title { margin: 0 0 18px; font-size: 18px; }
  .modal__grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
  }
  .modal__grid label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    opacity: 0.85;
  }
  .modal__grid label span { display: flex; align-items: center; gap: 6px; }
  .modal__grid input,
  .modal__grid select {
    background: rgba(255, 255, 255, 0.05);
    color: inherit;
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 13px;
    font-family: inherit;
  }
  .modal__checkbox {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .modal__checkbox input { width: auto; }
  .hint { opacity: 0.55; font-style: normal; font-size: 11px; }
  .modal__error {
    color: #ff8484;
    background: rgba(255, 132, 132, 0.08);
    border: 1px solid rgba(255, 132, 132, 0.25);
    border-radius: 6px;
    padding: 8px 12px;
    margin: 16px 0 0;
    font-size: 12px;
  }
  .modal__actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 20px;
  }
  .btn-cancel, .btn-save {
    border-radius: 8px;
    padding: 9px 18px;
    font-size: 13px;
    cursor: pointer;
    border: 1px solid transparent;
  }
  .btn-cancel { background: rgba(255, 255, 255, 0.06); color: inherit; border-color: rgba(255, 255, 255, 0.12); }
  .btn-save { background: #4b9eff; color: white; }
  .btn-save:disabled, .btn-cancel:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
