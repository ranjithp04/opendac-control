<script lang="ts" generics="T extends string | number">
  let {
    options,
    value,
    onpick,
    label,
    height = 30,
    fontSize = 12,
    mono = false,
    trackBg = 'var(--card)',
    disabled = false,
  }: {
    options: { value: T; label: string }[];
    value: T | null;
    onpick: (v: T) => void;
    label: string;
    height?: number;
    fontSize?: number;
    mono?: boolean;
    trackBg?: string;
    disabled?: boolean;
  } = $props();
</script>

<div
  class="seg"
  role="radiogroup"
  aria-label={label}
  style:grid-template-columns="repeat({options.length}, minmax(0, 1fr))"
  style:background={trackBg}
>
  {#each options as o (o.value)}
    <button
      type="button"
      role="radio"
      aria-checked={o.value === value}
      class:sel={o.value === value}
      class:mono
      {disabled}
      style:height="{height}px"
      style:font-size="{fontSize}px"
      onclick={() => onpick(o.value)}
    >
      {o.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    display: grid;
    gap: 4px;
    padding: 3px;
    border-radius: 8px;
  }
  button {
    border: none;
    border-radius: 6px;
    cursor: pointer;
    background: transparent;
    color: var(--text-2);
  }
  button.sel {
    background: var(--sel-bg);
    color: var(--text);
  }
  button.mono {
    font-family: var(--mono);
    font-weight: 500;
  }
  button:disabled {
    opacity: 0.45;
  }
</style>
