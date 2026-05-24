<script lang="ts">
  import type { ChatMessage, TypingNotification } from "$lib/api";
  import { msgKey } from "$lib/utils/chat";

  interface Props {
    messages: ChatMessage[];
    localRole: string;
    typing: TypingNotification | null;
    listEl: HTMLDivElement | null;
    emptyText?: string;
    typingText?: string;
    extraClass?: string;
  }

  let {
    messages,
    localRole,
    typing,
    listEl = $bindable(),
    emptyText = "Aucun message pour l'instant. Envoie le premier !",
    typingText = "est en train d'écrire…",
    extraClass = ""
  }: Props = $props();
</script>

<div class="rd-chat__list {extraClass}" bind:this={listEl}>
  {#if messages.length === 0}
    <p class="rd-empty">{emptyText}</p>
  {:else}
    {#each messages as msg (msgKey(msg))}
      {@const mine = (msg.senderRole ?? msg.senderName) === localRole}
      {@const isAi = msg.senderName === "Agent IA" || msg.senderName === "Technicien (IA)"}
      <div class="rd-chat__row" class:rd-chat__row--mine={mine}>
        <div class="rd-chat__bubble" class:rd-chat__bubble--mine={mine} class:rd-chat__bubble--ai={isAi}>
          <div class="rd-chat__meta">
            <span class="rd-chat__sender">{mine ? "Moi" : (isAi ? "🤖 IA" : (msg.senderName === "agent" ? "PC distant" : "Technicien"))}</span>
            <span class="rd-chat__ts">{new Date(msg.timestamp).toLocaleTimeString()}</span>
          </div>
          <p class="rd-chat__text">{msg.content}</p>
        </div>
      </div>
    {/each}
  {/if}
</div>

{#if typing && typing.senderRole !== localRole}
  <p class="rd-chat__typing">
    <span class="rd-chat__typing-dot"></span>
    <span class="rd-chat__typing-dot"></span>
    <span class="rd-chat__typing-dot"></span>
    <span>{typing.senderRole === "agent" ? "PC distant" : "Technicien"} {typingText}</span>
  </p>
{/if}
