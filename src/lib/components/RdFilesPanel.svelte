<script lang="ts">
  import type { ControlSession } from "$lib/api";
  import { fileChannel } from "$lib/managers/file-channel.svelte";
  import RdTransferList from "./RdTransferList.svelte";

  interface Props {
    session: ControlSession;
    actionLoading: boolean;
    fileInputEl: HTMLInputElement | null;
    onBackToMenu: () => void;
    onTriggerPicker: () => void;
    onFilePicked: (e: Event) => void;
    onDisconnect: () => void;
  }

  let {
    session,
    actionLoading,
    fileInputEl = $bindable(),
    onBackToMenu,
    onTriggerPicker,
    onFilePicked,
    onDisconnect
  }: Props = $props();
</script>

<section class="rd-panel">
  <header class="rd-session-menu__head">
    <div>
      <h2 class="rd-panel__title"><span class="rd-icon">📄</span> Transfert de fichiers</h2>
      <p class="rd-viewer__sub">
        Vidéo désactivée — toute la bande passante est dédiée au transfert.
        {#if fileChannel.rdFileChannelLive}
          <span style="color:#4ade80">● Canal P2P ouvert</span>
        {:else}
          <span style="color:#fbbf24">● Canal en attente…</span>
        {/if}
      </p>
    </div>
    <div class="rd-viewer__actions">
      <button class="rd-viewer__btn" type="button" onclick={onBackToMenu}>← Menu</button>
      <button
        class="rd-viewer__btn"
        type="button"
        onclick={onTriggerPicker}
        disabled={!fileChannel.rdFileChannelLive}>📤 Envoyer fichier</button>
      <button
        class="rd-viewer__disconnect"
        type="button"
        onclick={onDisconnect}
        disabled={actionLoading}>Déconnecter</button>
    </div>
  </header>

  <input bind:this={fileInputEl} type="file" multiple style="display:none" onchange={onFilePicked} />

  {#if Object.keys(fileChannel.fileTransfers).length === 0}
    <p class="rd-empty">Aucun transfert pour l'instant. Clique "Envoyer fichier" pour démarrer.</p>
  {:else}
    <RdTransferList
      transfers={fileChannel.fileTransfers}
      variant="verbose"
      peerName={session.agentMachineId ?? null} />
  {/if}
</section>
