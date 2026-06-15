<script lang="ts">
  import {
    copyText,
    getConnectQr,
    startBox,
    stopBox,
    type ConnectQr,
    type Status,
  } from "$lib/api";
  import PeoplePanel from "./PeoplePanel.svelte";
  import SettingsPanel from "./SettingsPanel.svelte";
  import BoxesPanel from "./BoxesPanel.svelte";

  let { st }: { st: Status } = $props();

  type View = "home" | "people" | "boxes" | "agent" | "settings";
  let view = $state<View>("home");

  let qr = $state<ConnectQr | null>(null);
  let showQr = $state(false);
  let qrBusy = $state(false);
  let copied = $state(false);
  let powerBusy = $state(false);
  let toast = $state("");
  let toastTimer: ReturnType<typeof setTimeout> | null = null;

  // Boxes (pairing) + Agent (MCP) land in later builds.
  const nav: { key: View; label: string; soon?: boolean }[] = [
    { key: "home", label: "Home" },
    { key: "people", label: "People" },
    { key: "boxes", label: "Boxes" },
    { key: "agent", label: "Agent", soon: true },
    { key: "settings", label: "Settings" },
  ];

  const actionCards: { title: string; blurb: string; go?: View }[] = [
    { title: "Add a person", blurb: "Invite someone to message you.", go: "people" },
    { title: "Pair with a box", blurb: "Link up with a friend's box.", go: "boxes" },
    { title: "Back up now", blurb: "Keep a fresh copy of your keys.", go: "settings" },
  ];

  // Map a service state to its status-dot class (healthy=green, starting=amber,
  // stopped=dim, error=red). Shared by the always-on System row and the error list.
  function dotClass(state: string) {
    return state === "healthy"
      ? "dot-ok"
      : state === "starting"
        ? "dot-warn"
        : state === "error"
          ? "dot-err"
          : "dim";
  }

  function showToast(msg: string) {
    toast = msg;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ""), 2600);
  }

  function comingSoon() {
    showToast("Coming in the next build");
  }

  function onAction(go?: View) {
    if (go) view = go;
    else comingSoon();
  }

  async function copyAddress() {
    if (st.onion && (await copyText(st.onion))) {
      copied = true;
      setTimeout(() => (copied = false), 1800);
    }
  }

  async function toggleQr() {
    if (showQr) {
      showQr = false;
      return;
    }
    if (!qr) {
      qrBusy = true;
      try {
        qr = await getConnectQr();
      } catch {
        showToast("Couldn't build the QR code just yet — try again in a moment.");
        qrBusy = false;
        return;
      }
      qrBusy = false;
    }
    showQr = true;
  }

  async function power(action: "start" | "stop") {
    powerBusy = true;
    try {
      if (action === "start") await startBox();
      else await stopBox();
    } catch (e) {
      showToast(String(e));
    } finally {
      powerBusy = false;
    }
  }
</script>

<div class="shell">
  <nav class="rail" aria-label="Main">
    <div class="rail-brand" aria-hidden="true">&#10059;</div>
    {#each nav as item}
      {#if item.soon}
        <button class="rail-item" disabled title="Soon">
          {item.label}
          <span class="soon">soon</span>
        </button>
      {:else}
        <button
          class="rail-item"
          class:active={view === item.key}
          aria-current={view === item.key ? "page" : undefined}
          onclick={() => (view = item.key)}
        >
          {item.label}
        </button>
      {/if}
    {/each}
  </nav>

  {#if view === "people"}
    <main class="content"><PeoplePanel {st} /></main>
  {:else if view === "boxes"}
    <main class="content"><BoxesPanel {st} /></main>
  {:else if view === "settings"}
    <main class="content"><SettingsPanel {st} /></main>
  {:else}
  <main class="content">
    <header class="topline">
      <h1>{st.box_name || "Your box"}</h1>
      <p class="dim counts">
        {st.people_count}
        {st.people_count === 1 ? "person" : "people"} &middot; {st.paired_count}
        paired {st.paired_count === 1 ? "box" : "boxes"}
      </p>
    </header>

    {#if st.demo_mode}
      <div class="banner-demo" role="status">
        <span class="dot-warn" aria-hidden="true">&#9679;</span>
        <span>
          Demo mode — running without the real engine. Run
          <span class="mono">scripts/fetch-sidecars.sh</span>.
        </span>
      </div>
    {/if}

    <section class="card status-card" aria-live="polite">
      {#if st.phase === "running"}
        <p class="sentence">
          <span class="dot-ok" aria-hidden="true">&#9679;</span> All good. Reachable
          over the private network.
        </p>
        <button
          class="btn btn-subtle"
          onclick={() => power("stop")}
          disabled={powerBusy}
        >
          {powerBusy ? "Pausing…" : "Pause"}
        </button>
      {:else if st.phase === "stopped"}
        <p class="sentence">
          <span class="dim" aria-hidden="true">&#9702;</span> Your box is paused —
          people can't reach you.
        </p>
        <button
          class="btn btn-primary"
          onclick={() => power("start")}
          disabled={powerBusy}
        >
          {powerBusy ? "Starting…" : "Start"}
        </button>
      {:else if st.phase === "error"}
        <p class="sentence">
          <span class="dot-err" aria-hidden="true">&#9679;</span> Something's not
          right — your box hit an error.
        </p>
        <button
          class="btn btn-subtle"
          onclick={() => power("start")}
          disabled={powerBusy}
        >
          {powerBusy ? "Trying…" : "Try again"}
        </button>
      {:else}
        <p class="sentence">
          <span class="dot-warn" aria-hidden="true">&#9679;</span> Finishing setup —
          almost there…
        </p>
      {/if}
    </section>

    {#if st.phase === "error" && st.services.length}
      <ul class="services">
        {#each st.services as svc}
          <li>
            <span aria-hidden="true" class={dotClass(svc.state)}>&#9679;</span>
            {svc.name} — {svc.state}
          </li>
        {/each}
      </ul>
    {/if}

    {#if st.services.length}
      <div class="system-row" aria-label="System services">
        <span class="system-label dim">System</span>
        {#each st.services as svc}
          <span class="system-svc" title="{svc.name} — {svc.state}">
            <span aria-hidden="true" class={dotClass(svc.state)}>&#9679;</span>
            {svc.name}
          </span>
        {/each}
      </div>
    {/if}

    <section class="card address-card">
      <div class="address-head">
        <h2>Your private address</h2>
        <div class="address-actions">
          <button
            class="btn-mini"
            onclick={copyAddress}
            disabled={!st.onion}
            aria-label="Copy your private address"
            title="Copy your private address"
          >
            {copied ? "✓ copied" : "copy"}
          </button>
          <button
            class="btn-mini"
            onclick={toggleQr}
            disabled={qrBusy}
            aria-expanded={showQr}
            aria-label={showQr ? "Hide QR code" : "Show QR code"}
          >
            {qrBusy ? "…" : showQr ? "hide QR" : "show QR"}
          </button>
        </div>
      </div>
      {#if st.onion}
        <p class="mono address">{st.onion}</p>
      {:else}
        <div class="address-minting" aria-live="polite">
          <span class="spinner" aria-hidden="true"></span>
          <span class="dim"
            >Building your private address — usually under a minute.</span
          >
        </div>
      {/if}
      {#if showQr && qr}
        <div
          class="qr-box"
          role="img"
          aria-label="QR code for your private address"
        >
          {@html qr.svg}
        </div>
      {/if}
    </section>

    <section class="actions-grid" aria-label="Actions">
      {#each actionCards as a}
        <button class="card action-card" onclick={() => onAction(a.go)}>
          <span class="action-title">{a.title}</span>
          <span class="dim action-blurb">{a.blurb}</span>
        </button>
      {/each}
    </section>

    <footer class="foot dim">
      Your box runs while this computer is on — keep it awake and plugged in.
    </footer>
  </main>
  {/if}

  {#if toast}
    <div class="toast" role="status">{toast}</div>
  {/if}
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 13rem 1fr;
    min-height: 100vh;
  }

  /* ── left rail ── */

  .rail {
    border-right: 1px solid var(--hairline);
    padding: var(--sp-5) var(--sp-3);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    background: var(--surface);
  }

  .rail-brand {
    color: var(--accent);
    font-size: var(--fs-xl);
    padding: 0 var(--sp-3) var(--sp-4);
  }

  .rail-item {
    appearance: none;
    font: inherit;
    font-size: var(--fs-sm);
    font-weight: 600;
    text-align: left;
    color: var(--text-dim);
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    padding: 0.55rem var(--sp-3);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
  }

  .rail-item.active {
    color: var(--accent-ink);
    background: var(--accent);
  }

  .rail-item:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .soon {
    font-size: 0.65rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    border: 1px solid var(--hairline-strong);
    border-radius: 999px;
    padding: 0.05rem 0.4rem;
  }

  /* ── content ── */

  .content {
    padding: var(--sp-6);
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    max-width: 52rem;
  }

  .topline h1 {
    font-size: var(--fs-xl);
  }

  .counts {
    font-size: var(--fs-sm);
    margin-top: var(--sp-1);
  }

  .banner-demo {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    border: 1px solid var(--warn);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    color: var(--text);
    border-radius: var(--radius);
    padding: var(--sp-3) var(--sp-4);
    font-size: var(--fs-sm);
  }

  .status-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
  }

  .sentence {
    font-size: var(--fs-lg);
    font-weight: 600;
  }

  .services {
    list-style: none;
    margin: 0;
    padding: 0 var(--sp-2);
    display: flex;
    gap: var(--sp-5);
    font-size: var(--fs-sm);
    color: var(--text-dim);
  }

  .system-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-4);
    padding: 0 var(--sp-2);
    font-size: var(--fs-xs);
    color: var(--text-dim);
  }

  .system-label {
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .system-svc {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
  }

  .address-card h2 {
    font-size: var(--fs-md);
  }

  .address-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    margin-bottom: var(--sp-3);
  }

  .address-actions {
    display: flex;
    gap: var(--sp-2);
  }

  .address {
    color: var(--text);
    font-size: var(--fs-sm);
  }

  .address-minting {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--fs-sm);
  }

  .spinner {
    width: 0.9rem;
    height: 0.9rem;
    flex: none;
    border: 2px solid var(--hairline-strong);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .qr-box {
    margin-top: var(--sp-4);
    background: #fff;
    border-radius: var(--radius);
    padding: var(--sp-3);
    width: 12rem;
    height: 12rem;
  }

  .qr-box :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
  }

  .actions-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--sp-4);
  }

  .action-card {
    appearance: none;
    font: inherit;
    color: var(--text);
    text-align: left;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    transition: border-color 120ms ease;
  }

  .action-card:hover {
    border-color: var(--accent);
  }

  .action-title {
    font-weight: 650;
    font-size: var(--fs-md);
  }

  .action-blurb {
    font-size: var(--fs-sm);
  }

  .foot {
    font-size: var(--fs-xs);
    margin-top: var(--sp-4);
  }

  .toast {
    position: fixed;
    bottom: var(--sp-5);
    left: 50%;
    transform: translateX(-50%);
    background: var(--surface-raised);
    border: 1px solid var(--hairline-strong);
    color: var(--text);
    border-radius: 999px;
    padding: var(--sp-2) var(--sp-5);
    font-size: var(--fs-sm);
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.4);
  }
</style>
