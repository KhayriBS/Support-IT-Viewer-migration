<script lang="ts">
  import type { ChatMessage, TypingNotification } from "$lib/api";
  import RdChatList from "./RdChatList.svelte";
  import RdChatCompose from "./RdChatCompose.svelte";

  interface Props {
    chatConnected: boolean;
    chatError: string | null;
    chatLocalRole: string;
    messages: ChatMessage[];
    typing: TypingNotification | null;
    chatListEl: HTMLDivElement | null;
    chatInput: string;
    actionLoading: boolean;
    composeDisabled: boolean;
    onReconnect: () => void;
    onBackToMenu: () => void;
    onDisconnect: () => void;
    onSend: () => void;
    onInput: () => void;
  }

  let {
    chatConnected,
    chatError,
    chatLocalRole,
    messages,
    typing,
    chatListEl = $bindable(),
    chatInput = $bindable(),
    actionLoading,
    composeDisabled,
    onReconnect,
    onBackToMenu,
    onDisconnect,
    onSend,
    onInput
  }: Props = $props();
</script>

<section class="rd-panel rd-chat">
  <header class="rd-session-menu__head">
    <div>
      <h2 class="rd-panel__title"><span class="rd-icon">💬</span> Chat</h2>
      <p class="rd-viewer__sub">
        <span class="rd-chat__pill" class:rd-chat__pill--ok={chatConnected} class:rd-chat__pill--warn={!chatConnected}>
          {chatConnected ? "Connecté" : "Hors ligne"}
        </span>
        <span class="rd-chat__role">Vous êtes&nbsp;: <strong>{chatLocalRole === "agent" ? "PC distant" : "Technicien"}</strong></span>
      </p>
    </div>
    <div class="rd-viewer__actions">
      {#if !chatConnected}
        <button class="rd-viewer__btn" type="button" onclick={onReconnect}>Reconnecter</button>
      {/if}
      <button class="rd-viewer__btn" type="button" onclick={onBackToMenu}>← Menu</button>
      <button class="rd-viewer__disconnect" type="button" onclick={onDisconnect} disabled={actionLoading}>Déconnecter</button>
    </div>
  </header>

  {#if chatError}
    <p class="rd-chat__error">{chatError}</p>
  {/if}

  <RdChatList
    {messages}
    localRole={chatLocalRole}
    {typing}
    bind:listEl={chatListEl}
    typingText="est en train d'écrire…" />

  <RdChatCompose
    bind:value={chatInput}
    placeholder="Écris un message…"
    disabled={composeDisabled}
    {onSend}
    {onInput} />
</section>
