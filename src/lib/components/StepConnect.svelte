<script lang="ts">
  import { onMount } from "svelte";
  import { copyText, getConnectQr, type ConnectQr } from "$lib/api";

  let { onNext }: { onNext: () => void } = $props();

  let qr = $state<ConnectQr | null>(null);
  let copied = $state(false);

  const steps = [
    "Install the PurePrivacy app on your phone.",
    "Tap Scan in the app.",
    "Point your phone here, at the code.",
  ];

  onMount(() => {
    let alive = true;
    const load = async () => {
      if (!alive) return;
      try {
        qr = await getConnectQr();
      } catch {
        // address may still be settling — try again shortly
        setTimeout(load, 1200);
      }
    };
    void load();
    return () => {
      alive = false;
    };
  });

  async function copyPayload() {
    if (qr && (await copyText(qr.payload))) {
      copied = true;
      setTimeout(() => (copied = false), 1800);
    }
  }
</script>

<div class="step">
  <p class="eyebrow">Step 3 of 4 &middot; Connect your phone</p>
  <h1>Put your box in your pocket</h1>

  <div class="cols">
    <ol class="safety">
      {#each steps as s, i}
        <li class="card">
          <span class="num" aria-hidden="true">{i + 1}</span>
          <span>{s}</span>
        </li>
      {/each}
    </ol>

    <div class="qr-side">
      {#if qr}
        <div class="qr-box" role="img" aria-label="QR code for connecting your phone">
          {@html qr.svg}
        </div>
        <p class="byhand dim">
          or enter by hand:
          <span class="mono payload">{qr.payload}</span>
          <button
            class="btn-mini"
            onclick={copyPayload}
            aria-label="Copy connect address"
            title="Copy connect address"
          >
            {copied ? "✓ copied" : "copy"}
          </button>
        </p>
      {:else}
        <div class="qr-box qr-wait">
          <p class="dim">
            <span class="spin" aria-hidden="true">&#10227;</span> building your
            code…
          </p>
        </div>
      {/if}
    </div>
  </div>

  <div class="actions">
    <p class="dim later">
      You can connect your phone now, or any time later from the dashboard.
    </p>
    <button class="btn btn-primary" onclick={onNext}>Continue</button>
  </div>
</div>

<style>
  .step {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }

  h1 {
    font-size: var(--fs-xl);
  }

  .cols {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--sp-6);
    align-items: start;
    margin-top: var(--sp-2);
  }

  .safety {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  .safety li {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    padding: var(--sp-4);
  }

  .num {
    flex-shrink: 0;
    width: 2rem;
    height: 2rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 50%;
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 700;
  }

  .qr-side {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    max-width: 16rem;
  }

  .qr-box {
    background: #fff;
    border-radius: var(--radius);
    padding: var(--sp-3);
    width: 14rem;
    height: 14rem;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .qr-box :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
  }

  .qr-wait {
    background: var(--surface);
    border: 1px solid var(--hairline);
  }

  .byhand {
    font-size: var(--fs-xs);
  }

  .payload {
    color: var(--text);
    display: block;
    margin: var(--sp-1) 0;
  }

  .spin {
    display: inline-block;
    animation: spin 1.4s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .actions {
    margin-top: var(--sp-2);
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-3);
  }

  .later {
    font-size: var(--fs-sm);
    max-width: 36ch;
  }
</style>
