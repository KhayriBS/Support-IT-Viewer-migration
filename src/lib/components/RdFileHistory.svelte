<script lang="ts">
  import type { RdFileRow, RdFileFilter } from "$lib/types/ui";
  import { rdFormatBytes, rdFormatRelative, rdFileIconClass } from "$lib/utils/format";

  interface Props {
    entries: RdFileRow[];
    error: string | null;
    loading: boolean;
    search: string;
    filter: RdFileFilter;
  }

  let {
    entries,
    error,
    loading,
    search = $bindable(),
    filter = $bindable()
  }: Props = $props();
</script>

<section class="rd-panel rd-history">
  <header class="rd-history__head">
    <h2 class="rd-panel__title"><span class="rd-icon">📄</span> Historique des fichiers</h2>
    <span class="rd-history__count">{entries.length} fichier{entries.length > 1 ? "s" : ""}</span>
  </header>
  <input
    class="rd-history__search"
    type="search"
    placeholder="Rechercher par nom de fichier ou code machine..."
    bind:value={search} />
  <div class="rd-history__filters">
    <select class="rd-select" bind:value={filter}>
      <option value="all">Tous les transferts</option>
      <option value="upload">Fichiers envoyés</option>
      <option value="download">Fichiers reçus</option>
    </select>
  </div>
  <div class="rd-history__list">
    {#if error}
      <p class="rd-empty" style="color:#fca5a5">Erreur historique : {error}</p>
    {:else if entries.length === 0}
      <p class="rd-empty">{loading ? "Chargement…" : "Aucun transfert pour les filtres actuels."}</p>
    {:else}
      {#each entries as file (file.transferId)}
        <article class="rd-file">
          <span class="rd-file__icon {rdFileIconClass(file.fileName)}">📄</span>
          <div class="rd-file__body">
            <strong class="rd-file__name">{file.fileName}</strong>
            <p class="rd-file__sub">
              {file.type === "upload" ? "Envoyé vers" : "Reçu de"}
              <strong>{file.peerLabel}</strong>
            </p>
            <p class="rd-file__meta">
              {rdFormatBytes(file.sizeBytes)} &nbsp;•&nbsp; {rdFormatRelative(file.startedMs)}
              {#if file.state !== "complete"}
                &nbsp;•&nbsp; <span class="rd-file__state">{file.state}</span>
              {/if}
              {#if file.error}
                &nbsp;•&nbsp; <span class="rd-file__state" style="color:#fca5a5">{file.error}</span>
              {/if}
            </p>
          </div>
        </article>
      {/each}
    {/if}
  </div>
</section>

<style>
  .rd-file {
    display: flex;
    gap: 12px;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 10px;
    padding: 12px 14px;
  }
  .rd-file__icon {
    width: 36px;
    height: 36px;
    border-radius: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
    flex-shrink: 0;
  }
  .rd-file__icon--pdf { background: rgba(56, 189, 248, 0.15); color: #38bdf8; }
  .rd-file__icon--ppt { background: rgba(74, 222, 128, 0.15); color: #4ade80; }
  .rd-file__icon--zip { background: rgba(56, 189, 248, 0.15); color: #38bdf8; }
  .rd-file__body { flex: 1; min-width: 0; }
  .rd-file__name {
    display: block;
    color: #e2e8f0;
    font-size: 14px;
    margin-bottom: 2px;
  }
  .rd-file__sub {
    margin: 0 0 2px 0;
    font-size: 12px;
    color: #94a3b8;
  }
  .rd-file__meta {
    margin: 0;
    font-size: 11px;
    color: #64748b;
  }
  .rd-file__state {
    color: #38bdf8;
    text-transform: uppercase;
    font-weight: 600;
    letter-spacing: 0.5px;
  }
</style>
