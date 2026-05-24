<script lang="ts">
  interface Props {
    connectionCode: string;
    actionLoading: boolean;
    waitingForApproval: boolean;
    actionError: string | null;
    onConnect: () => void | Promise<void>;
  }

  let {
    connectionCode = $bindable(),
    actionLoading,
    waitingForApproval,
    actionError,
    onConnect
  }: Props = $props();
</script>

<section class="rd-panel">
  <h2 class="rd-panel__title">Connexion par code</h2>
  <div class="rd-connect">
    <input
      class="rd-connect__input"
      type="text"
      placeholder="Entrez le code de l'ordinateur distant"
      bind:value={connectionCode}
      disabled={actionLoading || waitingForApproval}
      onkeydown={(e) => { if (e.key === "Enter" && !actionLoading) void onConnect(); }} />
    <button
      class="rd-connect__btn"
      type="button"
      onclick={() => void onConnect()}
      disabled={actionLoading || waitingForApproval || !connectionCode.trim()}>
      {actionLoading ? "Connexion…" : "Se connecter →"}
    </button>
  </div>

  {#if waitingForApproval}
    <div class="rd-connect__status rd-connect__status--waiting">
      <span class="rd-spinner"></span>
      <div>
        <strong>En attente d'acceptation…</strong>
        <p>L'ordinateur distant doit autoriser la connexion (clavier, souris, transfert de fichiers).</p>
      </div>
    </div>
  {:else if actionError}
    <div class="rd-connect__status rd-connect__status--error">
      <strong>Erreur :</strong> {actionError}
    </div>
  {/if}
</section>
