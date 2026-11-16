<script lang="ts">
  import { LatestSender } from '../lib/sender.svelte';
  import { pct } from '../lib/format';

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

  const sender = new LatestSender<number>((v) => oncommit(v));
  let active = $state(false);
  let local = $state(0);
  // While dragging (and until the last write is done) show the local value, so
  // incoming snapshots do not make the thumb jump.
  const shown = $derived(active || sender.busy ? local : value);
  $effect.pre(() => {
    live = active || sender.busy ? local : null;
  });

  function down() {
    local = value;
    active = true;
  }
  function input(e: Event & { currentTarget: HTMLInputElement }) {
    local = Number(e.currentTarget.value);
    active = true;
    sender.push(local);
  }
  function release() {
    active = false;
    sender.flush();
  }
</script>

<input
  class="hs"
  type="range"
  {min}
  {max}
  {step}
  value={shown}
  {disabled}
  aria-label={label}
  style:--p={pct(shown, min, max)}
  onpointerdown={down}
  oninput={input}
  onchange={release}
  onpointerup={release}
  onpointercancel={release}
  onblur={release}
/>
