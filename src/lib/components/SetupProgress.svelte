<script lang="ts">
  import Wordmark from "./Wordmark.svelte";
  import type { Status } from "$lib/api";

  let { st }: { st: Status | null } = $props();

  const stages = [
    { key: "starting_services", label: "Starting your box…" },
    { key: "minting_address", label: "Building your private address…" },
    { key: "ready", label: "Ready" },
  ] as const;

  const currentIndex = $derived(
    st?.phase === "running" || st?.setup_stage === "ready"
      ? stages.length
      : Math.max(
          stages.findIndex((s) => s.key === st?.setup_stage),
          0
        )
  );

  const headline = $derived(
    currentIndex >= stages.length ? "Ready" : stages[currentIndex].label
  );
</script>

<div class="wrap">
  <div class="card progress" role="status" aria-live="polite">
    <Wordmark size="md" />
    <h1>{headline}</h1>

    <ol class="stages">
      {#each stages as s, i}
        <li>
          {#if i < currentIndex}
            <span class="mark ok" aria-hidden="true">&#10003;</span>
            <span>{s.label.replace("…", "")}</span>
            <span class="visually-hidden">— done</span>
          {:else if i === currentIndex}
            <span class="mark spin" aria-hidden="true">&#10227;</span>
            <span>{s.label}</span>
          {:else}
            <span class="mark dim" aria-hidden="true">&#9675;</span>
            <span class="dim">{s.label.replace("…", "")}</span>
            <span class="visually-hidden">— waiting</span>
          {/if}
        </li>
      {/each}
    </ol>

    <p class="signature dim">Private, and a little slower — that's the deal.</p>
  </div>
</div>

<style>
  .wrap {
    min-height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--sp-6);
  }

  .progress {
    width: min(28rem, 100%);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    padding: var(--sp-6);
  }

  h1 {
    font-size: var(--fs-lg);
  }

  .stages {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  .stages li {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
  }

  .mark {
    width: 1.2em;
    text-align: center;
    flex-shrink: 0;
  }

  .ok {
    color: var(--ok);
    font-weight: 700;
  }

  .spin {
    display: inline-block;
    color: var(--warn);
    animation: spin 1.4s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .signature {
    font-size: var(--fs-sm);
    font-style: italic;
  }

  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>
