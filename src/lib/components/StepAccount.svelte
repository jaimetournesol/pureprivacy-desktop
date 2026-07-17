<script lang="ts">
  import { onMount } from "svelte";
  import { beginSetup, copyText, hasTauri, suggestPassword } from "$lib/api";
  import { mapError } from "$lib/errors";

  let { onNext }: { onNext: () => void } = $props();

  let username = $state("");
  let boxNameRaw = $state("");
  let boxNameTouched = $state(false);
  let password = $state("");
  let busy = $state(false);
  let error = $state("");
  let copied = $state(false);

  const boxName = $derived(
    boxNameTouched ? boxNameRaw : username ? `${username}'s box` : ""
  );

  async function newPassword() {
    if (!hasTauri()) return;
    try {
      password = await suggestPassword();
    } catch (e) {
      console.error("suggestPassword failed:", e);
      error = mapError(e);
    }
  }

  onMount(() => {
    void newPassword();
  });

  async function copyPassword() {
    if (await copyText(password)) {
      copied = true;
      setTimeout(() => (copied = false), 1800);
    }
  }

  function editBoxName(e: Event) {
    boxNameTouched = true;
    boxNameRaw = (e.currentTarget as HTMLInputElement).value;
  }

  async function continueSetup(e: SubmitEvent) {
    e.preventDefault();
    if (!username.trim() || !password) return;
    busy = true;
    error = "";
    try {
      await beginSetup({
        boxName: boxName.trim() || `${username.trim()}'s box`,
        username: username.trim(),
        password,
      });
      onNext();
    } catch (e) {
      console.error("beginSetup failed:", e);
      error = mapError(e);
    } finally {
      busy = false;
    }
  }
</script>

<form class="step" onsubmit={continueSetup}>
  <p class="eyebrow">Step 1 of 3 &middot; Your account</p>
  <h1>A name for you, a name for your box</h1>

  <div class="field">
    <label class="field-label" for="acct-boxname">Box name</label>
    <input
      id="acct-boxname"
      class="input"
      type="text"
      value={boxName}
      oninput={editBoxName}
      placeholder="e.g. The kitchen box"
      autocomplete="off"
    />
  </div>

  <div class="field">
    <label class="field-label" for="acct-username">Username</label>
    <input
      id="acct-username"
      class="input"
      type="text"
      bind:value={username}
      placeholder="e.g. maria"
      autocomplete="off"
      spellcheck="false"
    />
  </div>

  <div class="field">
    <label class="field-label" for="acct-password">Your password (we picked a strong one)</label>
    <div class="pw-row">
      <input
        id="acct-password"
        class="input mono"
        type="text"
        readonly
        value={password}
        aria-label="Generated password"
      />
      <button
        type="button"
        class="btn-mini"
        onclick={newPassword}
        aria-label="Generate a new password"
        title="Generate a new password"
      >
        &#8635; new one
      </button>
      <button
        type="button"
        class="btn-mini"
        onclick={copyPassword}
        aria-label="Copy password"
        title="Copy password"
      >
        {copied ? "✓ copied" : "copy"}
      </button>
    </div>
  </div>

  {#if error}
    <p class="error" role="alert">
      <span class="dot-err" aria-hidden="true">&#9679;</span> {error}
    </p>
  {/if}

  <div class="actions">
    <button
      type="submit"
      class="btn btn-primary"
      disabled={busy || !username.trim() || !password}
    >
      {busy ? "Starting…" : "Continue"}
    </button>
  </div>
</form>

<style>
  .step {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  h1 {
    font-size: var(--fs-xl);
  }

  .field {
    max-width: 32rem;
  }

  .pw-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  .error {
    color: var(--err);
    font-size: var(--fs-sm);
  }

  .actions {
    margin-top: var(--sp-2);
  }
</style>
