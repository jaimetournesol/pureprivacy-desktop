<script lang="ts">
  let { size = "md" }: { size?: "sm" | "md" | "lg" } = $props();
</script>

<span class="wordmark {size}">
  <span class="wordmark-glyph" aria-hidden="true">&#10059;</span>
  <span class="name">PurePrivacy</span>
</span>

<style>
  .wordmark {
    display: inline-flex;
    align-items: baseline;
    gap: 0.45em;
    font-weight: 700;
    letter-spacing: -0.01em;
    color: var(--text);
  }

  .sm {
    font-size: var(--fs-md);
  }

  .md {
    font-size: var(--fs-lg);
  }

  .lg {
    font-size: var(--fs-hero);
  }
</style>
