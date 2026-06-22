<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import {
    copyText,
    pairAccept,
    pairCreate,
    pairList,
    pairRemove,
    type PairCodeOut,
    type Pairing,
    type Status,
  } from "$lib/api";
  import { mapError } from "$lib/errors";

  let { st }: { st: Status } = $props();

  let peers = $state<Pairing[]>([]);
  let mine = $state<PairCodeOut | null>(null);
  let mineBusy = $state(false);
  let theirs = $state("");
  let acceptBusy = $state(false);
  let msg = $state("");
  let err = $state("");
  let copied = $state(false);
  // Which peer row is mid-confirm for removal (null = none). Two-step so a
  // single stray click never silently un-pairs a box.
  let confirmingOnion = $state<string | null>(null);

  // Pair codes carry a 15-minute expiry (pairing.rs CODE_TTL_SECS). Tick a live
  // countdown down from 15:00 once a code is created so it never looks stale.
  const CODE_TTL_SECS = 15 * 60;
  let secsLeft = $state(0);
  let ticker: ReturnType<typeof setInterval> | null = null;

  function startCountdown() {
    secsLeft = CODE_TTL_SECS;
    if (ticker) clearInterval(ticker);
    ticker = setInterval(() => {
      secsLeft = Math.max(0, secsLeft - 1);
      if (secsLeft === 0 && ticker) {
        clearInterval(ticker);
        ticker = null;
      }
    }, 1000);
  }

  function mmss(s: number) {
    const m = Math.floor(s / 60);
    const r = s % 60;
    return `${m}:${String(r).padStart(2, "0")}`;
  }

  onDestroy(() => {
    if (ticker) clearInterval(ticker);
  });

  async function refresh() {
    try {
      peers = await pairList();
    } catch (e) {
      console.error("pairList failed:", e);
      err = mapError(e);
    }
  }
  onMount(refresh);

  async function createCode() {
    mineBusy = true;
    err = "";
    try {
      mine = await pairCreate();
      startCountdown();
    } catch (e) {
      console.error("pairCreate failed:", e);
      err = mapError(e);
    } finally {
      mineBusy = false;
    }
  }

  async function copyCode() {
    if (mine && (await copyText(mine.code))) {
      copied = true;
      setTimeout(() => (copied = false), 1600);
    }
  }

  async function accept() {
    if (!theirs.trim()) return;
    acceptBusy = true;
    err = "";
    msg = "";
    try {
      const onion = await pairAccept(theirs.trim());
      msg = `Paired with ${short(onion)} — you can now message across boxes.`;
      theirs = "";
      await refresh();
    } catch (e) {
      console.error("pairAccept failed:", e);
      err = mapError(e);
    } finally {
      acceptBusy = false;
    }
  }

  async function remove(onion: string) {
    err = "";
    confirmingOnion = null;
    try {
      await pairRemove(onion);
      await refresh();
    } catch (e) {
      console.error("pairRemove failed:", e);
      err = mapError(e);
    }
  }

  function short(o: string) {
    return o.length > 22 ? `${o.slice(0, 10)}…${o.slice(-10)}` : o;
  }
</script>

<div class="panel">
  <header>
    <h1>People</h1>
    <p class="dim">
      Each person runs their own box. Pair with a friend's box to message across
      the two — you each share a code over a channel you trust, then read the
      address back to each other before you accept.
    </p>
  </header>

  {#if err}<p class="err">{err}</p>{/if}
  {#if msg}<p class="ok">{msg}</p>{/if}

  <div class="two">
    <section class="card">
      <h2>Your pair code</h2>
      <p class="dim sm">Hand this to your friend. It lasts 15 minutes.</p>
      {#if mine}
        <div class="qr" role="img" aria-label="Your pair code QR">
          {@html mine.svg}
        </div>
        <div class="codebox">
          <code class="mono">{mine.code}</code>
          <button class="btn-mini" onclick={copyCode}
            >{copied ? "✓" : "copy"}</button
          >
        </div>
        {#if secsLeft > 0}
          <p class="dim sm countdown" aria-live="polite">
            Expires in <span class="mono">{mmss(secsLeft)}</span>
          </p>
        {:else}
          <p class="expired" role="status">
            This code has expired — generate a fresh one.
          </p>
        {/if}
        <button class="btn btn-subtle" onclick={createCode} disabled={mineBusy}
          >Generate a fresh code</button
        >
      {:else}
        <button class="btn btn-primary" onclick={createCode} disabled={mineBusy}>
          {mineBusy ? "…" : "Create a pair code"}
        </button>
      {/if}
    </section>

    <section class="card">
      <h2>Accept a code</h2>
      <p class="dim sm">Paste the code your friend gave you.</p>
      <textarea
        class="input"
        rows="4"
        bind:value={theirs}
        placeholder="Paste a pair code…"
      ></textarea>
      <button
        class="btn btn-primary"
        onclick={accept}
        disabled={acceptBusy || !theirs.trim()}
      >
        {acceptBusy ? "Pairing…" : "Accept"}
      </button>
    </section>
  </div>

  <section class="card">
    <h2>Paired boxes ({peers.length})</h2>
    {#if peers.length === 0}
      <p class="dim sm">No paired boxes yet.</p>
    {:else}
      <ul class="peers">
        {#each peers as p}
          <li>
            <span class="dot-ok" aria-hidden="true">&#9679;</span>
            <span class="mono" title={p.onion}>{short(p.onion)}</span>
            {#if confirmingOnion === p.onion}
              <div class="confirm" role="group" aria-label="Confirm remove">
                <span class="confirm-q">Stop messaging this box?</span>
                <button class="btn-mini danger" onclick={() => remove(p.onion)}
                  >Remove</button
                >
                <button class="btn-mini" onclick={() => (confirmingOnion = null)}
                  >Cancel</button
                >
              </div>
            {:else}
              <button
                class="btn-mini"
                onclick={() => (confirmingOnion = p.onion)}>remove</button
              >
            {/if}
          </li>
          {#if confirmingOnion === p.onion}
            <li class="confirm-note dim sm">
              You'll need to pair again to reconnect.
            </li>
          {/if}
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  header h1 {
    font-size: var(--fs-xl);
  }
  header .dim {
    font-size: var(--fs-sm);
    margin-top: var(--sp-1);
    max-width: 50ch;
  }
  h2 {
    font-size: var(--fs-md);
  }
  .sm {
    font-size: var(--fs-sm);
    margin: var(--sp-1) 0 var(--sp-3);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-4);
    align-items: start;
  }
  .qr {
    background: #fff;
    border-radius: var(--radius);
    padding: var(--sp-3);
    width: 9rem;
    height: 9rem;
    margin-bottom: var(--sp-3);
  }
  .qr :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
  }
  .codebox {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-3);
  }
  .codebox code {
    font-size: var(--fs-xs);
    word-break: break-all;
    background: var(--surface-raised);
    border-radius: var(--radius-sm);
    padding: var(--sp-2);
    flex: 1;
  }
  textarea.input {
    width: 100%;
    resize: vertical;
    font-family: var(--mono);
    font-size: var(--fs-xs);
    margin-bottom: var(--sp-3);
  }
  .peers {
    list-style: none;
    margin: var(--sp-2) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .peers li {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    font-size: var(--fs-sm);
  }
  .peers .mono {
    flex: 1;
  }
  .confirm {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .confirm-q {
    font-size: var(--fs-sm);
  }
  .btn-mini.danger {
    color: var(--err);
  }
  .confirm-note {
    margin: calc(-1 * var(--sp-1)) 0 0;
    padding-left: 1.4rem;
  }
  .countdown {
    margin: 0 0 var(--sp-3);
  }
  .expired {
    color: var(--warn);
    font-size: var(--fs-sm);
    margin: 0 0 var(--sp-3);
  }
  .err {
    color: var(--err);
    font-size: var(--fs-sm);
  }
  .ok {
    color: var(--ok);
    font-size: var(--fs-sm);
  }
</style>
