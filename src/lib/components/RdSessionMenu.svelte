<script lang="ts">
  import type { ControlSession } from "$lib/api";

  type Feature = "screen" | "chat" | "files";

  interface Props {
    session: ControlSession;
    chatLocalRole: "viewer" | "agent" | string;
    actionLoading: boolean;
    /** Identifiant du correspondant — calculé côté parent selon qui regarde. */
    peerLabel?: string;
    onPickFeature: (feature: Feature) => void;
    onDisconnect: () => void;
    /** Quitter la vue session sans terminer côté serveur (cible uniquement). */
    onBackToInterface?: () => void;
  }

  let { session, chatLocalRole, actionLoading, peerLabel, onPickFeature, onDisconnect, onBackToInterface }: Props = $props();

  const displayedPeer = $derived(peerLabel?.trim() || session.agentMachineId);
</script>

<section class="rd-panel">
  <header class="rd-session-menu__head">
    <div>
      <h2 class="rd-panel__title">
        <span class="rd-icon">🔗</span>
        Session établie avec
        <strong class="rd-viewer__peer">{displayedPeer}</strong>
      </h2>
      <p class="rd-viewer__sub">Choisis quelle fonctionnalité utiliser. La vidéo ne démarre que si tu cliques "Écran".</p>
    </div>
    {#if chatLocalRole !== "agent"}
      <button
        class="rd-viewer__disconnect menu-disconnect"
        type="button"
        onclick={onDisconnect}
        disabled={actionLoading}>
        Déconnecter
      </button>
    {:else if onBackToInterface}
      <button
        class="menu-back"
        type="button"
        onclick={onBackToInterface}>
        ← Retour
      </button>
    {/if}
  </header>

  <div class="rd-features" class:rd-features--single={chatLocalRole === "agent"}>
    {#if chatLocalRole !== "agent"}
      <button class="rd-feature" type="button" onclick={() => onPickFeature("screen")}>
        <span class="rd-feature__icon">🖥</span>
        <strong>Écran</strong>
        <span class="rd-feature__hint">Voir et contrôler le bureau distant</span>
      </button>
      <button class="rd-feature" type="button" onclick={() => onPickFeature("files")}>
        <span class="rd-feature__icon">📄</span>
        <strong>Transfert de fichiers</strong>
        <span class="rd-feature__hint">Envoyer/recevoir sans afficher l'écran</span>
      </button>
    {/if}
    <button class="rd-feature" type="button" onclick={() => onPickFeature("chat")}>
      <span class="rd-feature__icon">💬</span>
      <strong>Chat</strong>
      <span class="rd-feature__hint">
        {chatLocalRole === "agent"
          ? "Communiquer avec le technicien connecté"
          : "Échanger des messages"}
      </span>
    </button>
  </div>
</section>

<style>
  /* Style scoped : garantit que le bouton est visible même si app.css n'est
     pas rechargé après un rebuild (Vite/Tauri peut louper le hot-reload du
     CSS global). */
  .menu-disconnect {
    background: rgba(255, 132, 132, 0.16);
    color: #ff8484;
    border: 1px solid rgba(255, 132, 132, 0.35);
    border-radius: 8px;
    padding: 8px 16px;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
    font-family: inherit;
  }
  .menu-disconnect:hover {
    background: rgba(255, 132, 132, 0.28);
    border-color: rgba(255, 132, 132, 0.55);
  }
  .menu-disconnect:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .menu-back {
    background: rgba(255, 255, 255, 0.06);
    color: inherit;
    border: 1px solid rgba(255, 255, 255, 0.12);
    border-radius: 8px;
    padding: 8px 16px;
    font-size: 13px;
    cursor: pointer;
    font-family: inherit;
  }
  .menu-back:hover {
    background: rgba(255, 255, 255, 0.12);
  }
</style>
