<script lang="ts">
  interface Props {
    value: string;
    placeholder: string;
    disabled?: boolean;
    onSend: () => void;
    onInput?: () => void;
    /** Optional AI button (only shown when set). */
    onSendAi?: () => void;
    aiConnected?: boolean;
    aiBusy?: boolean;
    extraClass?: string;
  }

  let {
    value = $bindable(),
    placeholder,
    disabled = false,
    onSend,
    onInput,
    onSendAi,
    aiConnected = false,
    aiBusy = false,
    extraClass = ""
  }: Props = $props();

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (onSendAi && (e.ctrlKey || e.metaKey)) {
        onSendAi();
      } else {
        onSend();
      }
    }
  }
</script>

<div class="rd-chat__compose {extraClass}">
  <input
    class="rd-chat__input"
    type="text"
    {placeholder}
    bind:value
    {disabled}
    onkeydown={handleKeydown}
    oninput={onInput} />

  {#if onSendAi}
    <button
      class="rd-chat__send rd-chat__send-ai"
      type="button"
      onclick={onSendAi}
      disabled={!value.trim() || !aiConnected || aiBusy}
      title="Demander à l'IA (Ctrl+Entrée)">
      {aiBusy ? "…" : "🤖"}
    </button>
    <button
      class="rd-chat__send"
      type="button"
      onclick={onSend}
      disabled={!value.trim()}
      title="Envoyer au technicien (Entrée)">
      →
    </button>
  {:else}
    <button
      class="rd-chat__send"
      type="button"
      onclick={onSend}
      disabled={!value.trim()}>
      Envoyer
    </button>
  {/if}
</div>
