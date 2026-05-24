<script lang="ts">
  import type { FileTransfer } from "$lib/api/types";
  import { rdFormatBytes } from "$lib/utils/format";
  import { fileChannel } from "$lib/managers/file-channel.svelte";

  interface Props {
    transfers: Record<string, FileTransfer>;
    /** "compact" : overlay sur l'écran. "verbose" : panneau fichiers dédié (affiche chemin + bouton copier). */
    variant: "compact" | "verbose";
    /** Nom du PC distant — affiché dans le message de succès upload (verbose only). */
    peerName?: string | null;
    /** Affiche le header "Transferts de fichiers" + statut canal (compact only). */
    showHeader?: boolean;
    channelOpen?: boolean;
  }

  let {
    transfers,
    variant,
    peerName = null,
    showHeader = false,
    channelOpen = false
  }: Props = $props();

  const sortedTransfers = $derived(
    Object.values(transfers).sort((a, b) => b.startedAt - a.startedAt)
  );
</script>

<div class="rd-transfers" class:rd-transfers--inline={variant === "verbose"}>
  {#if showHeader}
    <div class="rd-transfers__head">
      <strong>Transferts de fichiers</strong>
      <span class="rd-transfers__hint">
        {channelOpen ? "Canal P2P ouvert" : "Canal en attente…"}
      </span>
    </div>
  {/if}

  {#each sortedTransfers as t (t.transferId)}
    <article class="rd-transfer rd-transfer--{t.state}">
      <div class="rd-transfer__icon">{t.type === "upload" ? "📤" : "📥"}</div>
      <div class="rd-transfer__body">
        <div class="rd-transfer__line">
          <strong class="rd-transfer__name">{t.fileName}</strong>
          <span class="rd-transfer__meta">
            {rdFormatBytes(t.doneBytes)} / {rdFormatBytes(t.totalSize)}
            {#if t.state === "active"}&nbsp;•&nbsp; {fileChannel.progressPercent(t)}%{/if}
          </span>
        </div>
        <div class="rd-transfer__bar">
          <div class="rd-transfer__bar-fill" style="width: {fileChannel.progressPercent(t)}%"></div>
        </div>

        {#if t.state === "error"}
          <p class="rd-transfer__error">Erreur : {t.error ?? "inconnue"}</p>
        {:else if t.state === "complete"}
          {#if variant === "verbose" && t.type === "upload"}
            {#if t.destPath}
              <p class="rd-transfer__done">
                ✓ Envoyé à <strong>{peerName ?? "l'autre PC"}</strong>
                ({(t.totalSize / 1024).toFixed(1)} KB)
              </p>
              <p class="rd-transfer__where">
                📁 <strong>Le fichier est sur l'AUTRE ordinateur</strong>
                ({peerName ?? "?"}), pas sur celui-ci.<br />
                Sur le PC distant, ouvre le dossier
                <strong>Téléchargements</strong> →
                <strong>LumiereTransfers</strong>
                (sous-dossier créé automatiquement).
                <br />
                Chemin complet :
                <code style="user-select:all">{t.destPath}</code>
                <button
                  class="rd-transfer__copy"
                  type="button"
                  onclick={() => navigator.clipboard?.writeText(t.destPath ?? "")}
                  title="Copier le chemin">📋 Copier</button>
              </p>
            {:else}
              <p class="rd-transfer__done rd-transfer__done--warn">
                ⚠ Tous les chunks envoyés mais pas d'ACK reçu de l'autre PC.
                L'agent distant ne confirme pas la réception — vérifie sa console
                (logs <code>[file-ch]</code>).
              </p>
            {/if}
          {:else if variant === "verbose" && t.type === "download"}
            <p class="rd-transfer__done">✓ Reçu et téléchargé localement</p>
          {:else}
            <p class="rd-transfer__done">
              {t.type === "upload" ? "Envoyé à l'autre PC" : "Reçu et téléchargé"}
            </p>
          {/if}
        {/if}
      </div>
      {#if t.state !== "active"}
        <button
          class="rd-transfer__close"
          type="button"
          onclick={() => fileChannel.dismissTransfer(t.transferId)}
          title="Retirer de la liste">×</button>
      {/if}
    </article>
  {/each}
</div>
