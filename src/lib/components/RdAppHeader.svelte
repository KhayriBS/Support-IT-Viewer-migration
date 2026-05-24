<script lang="ts">
  import { agentManager } from "$lib/managers/agent-manager.svelte";
</script>

<header class="rd-header">
  <div class="rd-title">
    <h1>Bureau à Distance</h1>
    <p>Contrôlez et partagez votre bureau en toute sécurité</p>
  </div>
  <div class="rd-machine-code">
    <span class="rd-machine-code__label">Code de cette machine</span>
    <span class="rd-machine-code__value">
      {#if agentManager.localConnectionCodeLoading && !agentManager.localConnectionCode}
        …
      {:else if agentManager.localConnectionCode}
        {agentManager.localConnectionCode}
      {:else}
        ------
      {/if}
    </span>
  </div>
</header>
