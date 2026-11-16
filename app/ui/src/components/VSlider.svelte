<script lang="ts">
  import { LatestSender } from '../lib/sender.svelte';

  let {
    value,
    min,
    max,
    step,
    label,
    oncommit,
    disabled = false,
    live = $bindable(null),
  }: {
    value: number;
    min: number;
    max: number;
    step: number;
    label: string;
    /** Called at most every 50 ms while dragging, and once on release. */
    oncommit: (v: number) => Promise<unknown>;
    disabled?: boolean;
    /** The value being dragged (or still being written), `null` otherwise; for labels and curves. */
    live?: number | null;
  } = $props();

  // Custom vertical slider: `writing-mode` range inputs are not reliable in the
  // macOS system webview.
  const THUMB = 16;
  const sender = new LatestSender<number>((v) => oncommit(v));
  let el: HTMLDivElement;
  let active = $state(false);
  let local = $state(0);
  const shown = $derived(active || sender.busy ? local : value);
  $effect.pre(() => {
    live = active || sender.busy ? local : null;
  });
  // 0 = top (max), 1 = bottom (min)
  const frac = $derived((max - shown) / (max - min));

  function clamp(v: number): number {
    return Math.min(max, Math.max(min, v));
  }
  function snap(v: number): number {
    return clamp(Math.round((v - min) / step) * step + min);
  }
  function setFromPointer(e: PointerEvent) {
    const r = el.getBoundingClientRect();
    const f = Math.min(1, Math.max(0, (e.clientY - r.top - THUMB / 2) / (r.height - THUMB)));
    const v = snap(max - f * (max - min));
    if (v !== local) {
      local = v;
      sender.push(v);
    }
  }
  function down(e: PointerEvent) {
    if (disabled || e.button !== 0) return;
    el.setPointerCapture(e.pointerId);
    local = value;
    active = true;
    setFromPointer(e);
  }
  function move(e: PointerEvent) {
    if (active) setFromPointer(e);
  }
  function up() {
    if (!active) return;
    active = false;
    sender.flush();
  }
  function key(e: KeyboardEvent) {
    if (disabled) return;
    let v: number;
    switch (e.key) {
      case 'ArrowUp':
      case 'ArrowRight':
        v = shown + step;
        break;
      case 'ArrowDown':
      case 'ArrowLeft':
        v = shown - step;
        break;
      case 'PageUp':
        v = shown + step * 4;
        break;
      case 'PageDown':
        v = shown - step * 4;
        break;
      case 'Home':
        v = min;
        break;
      case 'End':
        v = max;
        break;
      default:
        return;
    }
    e.preventDefault();
    local = snap(v);
    sender.push(local);
    sender.flush();
  }
</script>

<div
  bind:this={el}
  class="vs"
  class:disabled
  role="slider"
  tabindex={disabled ? -1 : 0}
  aria-label={label}
  aria-orientation="vertical"
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={shown}
  aria-disabled={disabled}
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  onkeydown={key}
>
  <span class="track"></span>
  <span class="thumb" style:top="calc({frac} * (100% - {THUMB}px))"></span>
</div>

<style>
  .vs {
    position: relative;
    width: 20px;
    height: 150px;
    cursor: pointer;
    touch-action: none;
  }
  .vs.disabled {
    cursor: default;
    opacity: 0.45;
  }
  .track {
    position: absolute;
    left: 8px;
    top: 0;
    bottom: 0;
    width: 4px;
    border-radius: 2px;
    background: var(--rail);
  }
  .thumb {
    position: absolute;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--thumb);
    border: 3px solid var(--thumb-ring);
  }
</style>
