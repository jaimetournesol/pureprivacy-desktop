<script lang="ts">
  import Wordmark from "./Wordmark.svelte";

  let { onNext }: { onNext: () => void } = $props();

  const bullets = [
    "Runs on a computer you own — not someone else's cloud",
    "Your messages stay between you and the people you choose",
    "Reachable only over a private network address",
    "If you lose the machine, your recovery kit brings everything back",
  ];
</script>

<div class="step">
  <Wordmark size="lg" />

  <h1>Your messages. Your hardware. Your keys.</h1>

  <ul class="bullets">
    {#each bullets as b}
      <li><span class="tick" aria-hidden="true">&#10003;</span> {b}</li>
    {/each}
  </ul>

  <div class="actions">
    <button class="btn btn-primary" onclick={onNext}>Set up my box</button>
    <span class="restore">
      <button class="btn-link" disabled title="Coming soon"
        >Restore from backup</button
      >
      <span class="dim soon-note">coming soon</span>
    </span>
  </div>
</div>

<style>
  .step {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    align-items: flex-start;
  }

  h1 {
    font-size: var(--fs-xl);
    max-width: 22ch;
  }

  .bullets {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    color: var(--text);
    font-size: var(--fs-md);
  }

  .tick {
    color: var(--ok);
    font-weight: 700;
    margin-right: var(--sp-2);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-5);
    margin-top: var(--sp-3);
  }

  .restore {
    display: inline-flex;
    align-items: baseline;
    gap: var(--sp-2);
  }

  .soon-note {
    font-size: var(--fs-xs);
  }
</style>
