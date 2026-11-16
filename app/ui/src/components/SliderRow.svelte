<script lang="ts">
  import HSlider from './HSlider.svelte';

  let {
    name,
    format,
    valueWidth = 40,
    live = $bindable(null),
    ...slider
  }: {
    /** Left caption (e.g. "Distance"); omit for none. */
    name?: string;
    /** Text for the value label; gets the value being dragged, if any. */
    format: (v: number) => string;
    /** Width of the value label in px. */
    valueWidth?: number;
    /** The value being dragged (or still being written), `null` otherwise. */
    live?: number | null;
    value: number;
    min: number;
    max: number;
    step: number;
    label: string;
    oncommit: (v: number) => Promise<unknown>;
    disabled?: boolean;
  } = $props();
</script>

<div class="row">
  {#if name}<span class="name">{name}</span>{/if}
  <HSlider {...slider} bind:live />
  <span class="val" style:width="{valueWidth}px">{format(live ?? slider.value)}</span>
</div>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .name {
    width: 56px;
    font-size: 11px;
    color: var(--text-2);
  }
  .val {
    width: 40px;
    text-align: right;
    font-family: var(--mono);
    font-weight: 500;
    font-size: 11px;
    color: var(--text-3);
  }
</style>
