<script lang="ts">
  // Vue USER : connexion par code + métriques + ses machines + historique.
  // Quand une session devient ACTIVE, +layout.svelte redirige vers /dashboard
  // qui orchestre l'UI vidéo/chat/fichiers ; à la fin de session, le router
  // re-cible /my-machines automatiquement.
  import { onDestroy, onMount } from "svelte";
  import { onAgentUpdate, technicianApi } from "$lib/api";
  import type { Agent } from "$lib/api/types";
  import type { RdFileRow } from "$lib/types/ui";
  import RdAppHeader from "$lib/components/RdAppHeader.svelte";
  import RdConnectPanel from "$lib/components/RdConnectPanel.svelte";
  import RdMetricsPanel from "$lib/components/RdMetricsPanel.svelte";
  import RdSessionHistory from "$lib/components/RdSessionHistory.svelte";
  import RdFileHistory from "$lib/components/RdFileHistory.svelte";
  import { agentManager } from "$lib/managers/agent-manager.svelte";
  import { historyManager } from "$lib/managers/history-manager.svelte";
  import { sessionManager } from "$lib/managers/session-manager.svelte";

  let machines = $state<Agent[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let lastRefresh = $state<string>("");
  let refreshTimer: ReturnType<typeof setInterval> | null = null;
  let unsubscribeRealtime: (() => void) | null = null;
  let connectionCode = $state("");

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

  // L'historiqueManager bifurque selon role : pour USER il utilise la clé
  // (machineId/connectionCode) résolue depuis agentManager. On déclenche
  // les fetchs dès qu'on a une clé valide ou qu'un filtre change.
  $effect(() => {
    void agentManager.localConnectionCode;
    void agentManager.localMachineId;
    void historyManager.sessionTypeFilter;
    void historyManager.sessionStatusFilter;
    void historyManager.sessionSearch;
    historyManager.scheduleSessionsRefresh();
  });

  $effect(() => {
    void agentManager.localConnectionCode;
    void agentManager.localMachineId;
    void historyManager.fileFilter;
    void historyManager.fileSearch;
    historyManager.scheduleFilesRefresh();
  });

  // Pas de canal P2P live ici (l'orchestration vidéo est sur /dashboard),
  // donc on alimente uniquement à partir de l'API.
  const rdFilteredFiles = $derived.by<RdFileRow[]>(() => {
    const search = historyManager.fileSearch.trim().toLowerCase();
    return historyManager.files
      .map((h) => {
        const peerLabel = h.peerLabel
          || (h.listDirection === "incoming" ? h.fromMachineId : h.toMachineId);
        const row: RdFileRow = {
          transferId: h.transferId,
          fileName: h.fileName,
          type: h.listDirection === "incoming" ? "download" : "upload",
          peerLabel,
          sizeBytes: h.fileSize,
          state:
            h.status === "COMPLETED" ? "complete"
              : h.status === "FAILED" ? "error"
              : h.status === "CANCELLED" ? "cancelled"
              : "active",
          error: h.errorMessage,
          startedMs: h.startedAt ? Date.parse(h.startedAt) : Date.now(),
          doneBytes: h.fileSize,
          isLive: false
        };
        return row;
      })
      .filter((f) => {
        if (historyManager.fileFilter !== "all" && f.type !== historyManager.fileFilter) return false;
        if (search && !f.fileName.toLowerCase().includes(search)
            && !f.peerLabel.toLowerCase().includes(search)) return false;
        return true;
      })
      .sort((a, b) => b.startedMs - a.startedMs);
  });

  onMount(() => {
    // Le sessionManager lit le code via cette callback (même pattern que
    // /dashboard) — sinon startSessionWithCode lit le `connectionCode` de
    // /dashboard et croit qu'il est vide.
    sessionManager.getConnectionCode = () => connectionCode;

    void refresh();
    refreshTimer = setInterval(refresh, 30_000);

    unsubscribeRealtime = onAgentUpdate((updated) => {
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

<main class="rd-page">
  <section class="rd-card">
    <RdAppHeader />

    <p class="meta">
      Connecté en tant que <strong>{agentManager.assignedUsername ?? "—"}</strong>
      <span class="dot"></span>
      Cette machine : <code>{agentManager.localMachineId}</code>
      {#if lastRefresh}
        <span class="dot"></span>
        Mis à jour à {lastRefresh}
      {/if}
    </p>

    <!-- ── Connexion par code (initie une session, redirige vers /dashboard) ── -->
    <RdConnectPanel
      bind:connectionCode
      actionLoading={sessionManager.actionLoading}
      waitingForApproval={sessionManager.waitingForApproval}
      actionError={sessionManager.actionError}
      onConnect={() => void sessionManager.startSessionWithCode()} />

    <!-- ── Métriques de cette machine ────────────────────────────────────── -->
    <RdMetricsPanel />

    <!-- ── Ses machines attribuées ───────────────────────────────────────── -->
    <section class="rd-panel mine">
      <header class="mine__head">
        <h2 class="rd-panel__title"><span class="rd-icon">🖥</span> Mes machines</h2>
        <span class="sub">{machines.length} machine{machines.length > 1 ? "s" : ""}</span>
      </header>

      {#if error}
        <p class="mine__error">Erreur : {error}</p>
      {/if}

      {#if loading && machines.length === 0}
        <p class="mine__info">Chargement…</p>
      {:else if machines.length === 0}
        <p class="mine__info">Aucune machine ne vous est attribuée pour le moment.</p>
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
      {/if}
      <p class="hint">
        Toute prise de contrôle à distance demande votre approbation via le
        pop-up qui s'affichera ici.
      </p>
    </section>

    <!-- ── Historique sessions + fichiers (machine courante) ─────────────── -->
    <div class="rd-history-grid">
      <RdSessionHistory
        entries={historyManager.sessions}
        error={historyManager.sessionsError}
        loading={historyManager.sessionsLoading}
        bind:search={historyManager.sessionSearch}
        bind:typeFilter={historyManager.sessionTypeFilter}
        bind:statusFilter={historyManager.sessionStatusFilter} />

      <RdFileHistory
        entries={rdFilteredFiles}
        error={historyManager.filesError}
        loading={historyManager.filesLoading}
        bind:search={historyManager.fileSearch}
        bind:filter={historyManager.fileFilter} />
    </div>
  </section>
</main>

<style>
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    opacity: 0.8;
    margin: 0 0 18px;
  }
  .dot {
    width: 4px; height: 4px; border-radius: 50%;
    background: rgba(255,255,255,0.3);
  }
  .mine {
    margin: 24px 0;
    padding: 20px;
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid rgba(255, 255, 255, 0.06);
    border-radius: 12px;
  }
  .mine__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }
  .sub { font-size: 12px; opacity: 0.7; }
  .mine__info { font-size: 13px; opacity: 0.7; }
  .mine__error { color: #ff8484; font-size: 13px; }
  .machines {
    width: 100%;
    border-collapse: collapse;
    background: rgba(255, 255, 255, 0.02);
    border-radius: 10px;
    overflow: hidden;
  }
  .machines th, .machines td {
    text-align: left;
    padding: 10px 14px;
    font-size: 13px;
    border-bottom: 1px solid rgba(255,255,255,0.06);
  }
  .machines th {
    background: rgba(255,255,255,0.04);
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
  .hint { margin-top: 12px; font-size: 12px; opacity: 0.6; }
</style>
