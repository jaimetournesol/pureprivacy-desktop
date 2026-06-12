<script lang="ts">
  import { onMount } from "svelte";
  import {
    startStatusPolling,
    stopStatusPolling,
    status,
    type Status,
  } from "$lib/api";
  import Wordmark from "$lib/components/Wordmark.svelte";
  import StepWelcome from "$lib/components/StepWelcome.svelte";
  import StepAccount from "$lib/components/StepAccount.svelte";
  import StepRecovery from "$lib/components/StepRecovery.svelte";
  import StepConnect from "$lib/components/StepConnect.svelte";
  import StepDone from "$lib/components/StepDone.svelte";
  import SetupProgress from "$lib/components/SetupProgress.svelte";
  import MainWindow from "$lib/components/MainWindow.svelte";

  /** 1=Welcome 2=Account 3=Recovery 4=Connect 5=Done */
  let step = $state(1);
  /** True once the user ran begin_setup in this session (past D2). */
  let setupStartedHere = $state(false);
  /** Set when the user clicks "Open PurePrivacy" on D5. */
  let openedMain = $state(false);

  onMount(() => {
    startStatusPolling();
    return () => stopStatusPolling();
  });

  type View = "loading" | "onboarding" | "progress" | "main";

  function pickView(s: Status | null): View {
    if (openedMain) return "main";
    if (!s) return "loading";
    switch (s.phase) {
      case "fresh":
        return "onboarding";
      case "setting_up":
        // Past D2 in this session: the onboarding screens already show
        // progress. Reopened mid-setup: show the centered progress card.
        return setupStartedHere ? "onboarding" : "progress";
      case "running":
      case "stopped":
      case "error":
        // Let the user finish D3–D5 if they started setup here.
        return setupStartedHere ? "onboarding" : "main";
      default:
        return "main";
    }
  }

  const view = $derived(pickView($status));
</script>

<svelte:head>
  <title>PurePrivacy</title>
</svelte:head>

{#if view === "loading"}
  <div class="splash">
    <Wordmark size="lg" />
    <p class="dim">Checking your box…</p>
  </div>
{:else if view === "onboarding"}
  <div class="onboard">
    <div class="onboard-inner">
      {#if step === 1}
        <StepWelcome onNext={() => (step = 2)} />
      {:else if step === 2}
        <StepAccount
          onNext={() => {
            setupStartedHere = true;
            step = 3;
          }}
        />
      {:else if step === 3}
        <StepRecovery st={$status} onNext={() => (step = 4)} />
      {:else if step === 4}
        <StepConnect onNext={() => (step = 5)} />
      {:else}
        <StepDone st={$status} onOpen={() => (openedMain = true)} />
      {/if}
    </div>
  </div>
{:else if view === "progress"}
  <SetupProgress st={$status} />
{:else if $status}
  <MainWindow st={$status} />
{/if}

<style>
  .splash {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-4);
  }

  .onboard {
    min-height: 100vh;
    display: flex;
    justify-content: center;
    padding: var(--sp-7) var(--sp-6);
  }

  .onboard-inner {
    width: min(40rem, 100%);
  }
</style>
