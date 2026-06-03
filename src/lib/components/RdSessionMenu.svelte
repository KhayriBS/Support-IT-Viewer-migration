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
  }

  let { session, chatLocalRole, actionLoading, peerLabel, onPickFeature, onDisconnect }: Props = $props();

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
    <button
      class="rd-viewer__disconnect"
      type="button"
      onclick={onDisconnect}
      disabled={actionLoading}>
      Déconnecter
    </button>
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
