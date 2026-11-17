<script lang="ts">
  import { LatestSender } from '../lib/sender.svelte';
  import { BAND_LABELS, db } from '../lib/format';

  let {
    bands,
    oncommit,
    disabled = false,
    live = $bindable(new Array<number | null>(10).fill(null)),
  }: {
    /** Gains in dB for the 10 bands, 31 Hz .. 16 kHz (what to draw; includes values being dragged). */
    bands: number[];
    /** Writes one band; called at most every 50 ms per band while dragging, and once on release. */
    oncommit: (band: number, gain: number) => Promise<unknown>;
    disabled?: boolean;
    /** Per band: the gain being dragged (or still being written), `null` otherwise. */
    live?: (number | null)[];
  } = $props();

  // The bands are an octave apart (31 Hz * 2^9 = 16 kHz), so a log frequency axis is evenly spaced.
  const W = 400;
  const H = 150;
  const LEFT = 30;
  const RIGHT = 12;
  const TOP = 8;
  const BOTTOM = 10;
  const RANGE = 12;
  const STEP = 0.5;
  const GRID = [12, 6, 0, -6, -12];

  const x = (i: number) => LEFT + (i / 9) * (W - LEFT - RIGHT);
  const y = (g: number) => TOP + ((RANGE - g) / (2 * RANGE)) * (H - TOP - BOTTOM);
  const clamp = (v: number) => Math.min(RANGE, Math.max(-RANGE, v));
  const snap = (v: number) => clamp(Math.round(v / STEP) * STEP);

  // Smooth curve through the band points (Catmull-Rom as cubic Beziers). Approximate: the
  // G6's real filter shapes are undocumented, so this only interpolates the band gains.
  const path = $derived.by(() => {
    const p = bands.map((g, i) => [x(i), y(clamp(g))] as const);
    let d = `M${p[0][0]},${p[0][1]}`;
    for (let i = 0; i < p.length - 1; i++) {
      const p0 = p[Math.max(0, i - 1)];
      const p1 = p[i];
      const p2 = p[i + 1];
      const p3 = p[Math.min(p.length - 1, i + 2)];
      const c1x = p1[0] + (p2[0] - p0[0]) / 6;
      const c1y = p1[1] + (p2[1] - p0[1]) / 6;
      const c2x = p2[0] - (p3[0] - p1[0]) / 6;
      const c2y = p2[1] - (p3[1] - p1[1]) / 6;
      d += ` C${c1x},${c1y} ${c2x},${c2y} ${p2[0]},${p2[1]}`;
    }
    return d;
  });

  const senders = Array.from(
    { length: 10 },
    (_, i) => new LatestSender<number>((g) => oncommit(i, g)),
  );
  let svg: SVGSVGElement;
  let active = $state<number | null>(null);
  let local = $state<number[]>(new Array(10).fill(0));
  $effect.pre(() => {
    live = senders.map((s, i) => (active === i || s.busy ? local[i] : null));
  });

  /** Gain (unsnapped) at the pointer's height. */
  function gainAt(e: PointerEvent): number {
    const r = svg.getBoundingClientRect();
    const sy = ((e.clientY - r.top) / r.height) * H;
    return RANGE - ((sy - TOP) / (H - TOP - BOTTOM)) * 2 * RANGE;
  }
  function put(i: number, g: number) {
    if (g === local[i]) return;
    local[i] = g;
    senders[i].push(g);
  }
  // Pointer gain minus the band's gain when the point was grabbed: a drag moves the band by the
  // pointer's movement, so pressing a point without moving does not change it.
  let grab = 0;
  function down(e: PointerEvent, i: number) {
    if (disabled || e.button !== 0) return;
    svg.setPointerCapture(e.pointerId);
    local[i] = bands[i];
    grab = gainAt(e) - clamp(bands[i]);
    active = i;
  }
  function move(e: PointerEvent) {
    if (active !== null) put(active, snap(gainAt(e) - grab));
  }
  function up() {
    if (active === null) return;
    const i = active;
    active = null;
    senders[i].flush();
  }
  function key(e: KeyboardEvent, i: number) {
    if (disabled) return;
    const cur = bands[i];
    let g: number;
    switch (e.key) {
      case 'ArrowUp':
      case 'ArrowRight':
        g = cur + STEP;
        break;
      case 'ArrowDown':
      case 'ArrowLeft':
        g = cur - STEP;
        break;
      case 'PageUp':
        g = cur + STEP * 4;
        break;
      case 'PageDown':
        g = cur - STEP * 4;
        break;
      case 'Home':
        g = -RANGE;
        break;
      case 'End':
        g = RANGE;
        break;
      default:
        return;
    }
    e.preventDefault();
    local[i] = cur;
    put(i, snap(g));
    senders[i].flush();
  }
</script>

<svg
  bind:this={svg}
  class="curve"
  class:disabled
  viewBox="0 0 {W} {H}"
  role="group"
  aria-label="Approximate EQ frequency response"
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
>
  <defs>
    <clipPath id="eqcurve-plot">
      <rect x={LEFT} y={TOP} width={W - LEFT - RIGHT} height={H - TOP - BOTTOM} />
    </clipPath>
  </defs>
  {#each GRID as g (g)}
    <line class="grid" class:zero={g === 0} x1={LEFT} x2={W - RIGHT} y1={y(g)} y2={y(g)} />
    <text class="axis" x={LEFT - 5} y={y(g)} text-anchor="end" dominant-baseline="middle">{db(g)}</text>
  {/each}
  {#each bands as _, i (i)}
    <line class="grid" x1={x(i)} x2={x(i)} y1={TOP} y2={H - BOTTOM} />
  {/each}
  <path class="line" d={path} clip-path="url(#eqcurve-plot)" />
  {#each bands as g, i (i)}
    <g
      class="pt"
      class:drag={active === i}
      role="slider"
      tabindex={disabled ? -1 : 0}
      aria-label="{BAND_LABELS[i]} Hz"
      aria-orientation="vertical"
      aria-valuemin={-RANGE}
      aria-valuemax={RANGE}
      aria-valuenow={g}
      aria-valuetext="{db(g)} dB"
      aria-disabled={disabled}
      onpointerdown={(e) => down(e, i)}
      onkeydown={(e) => key(e, i)}
    >
      <circle class="hit" cx={x(i)} cy={y(clamp(g))} r="12" />
      <circle class="dot" cx={x(i)} cy={y(clamp(g))} r="4.5" />
    </g>
  {/each}
</svg>

<style>
  .curve {
    display: block;
    width: 100%;
    height: auto;
    touch-action: none;
    user-select: none;
  }
  .grid {
    stroke: var(--border);
    stroke-width: 1;
  }
  .grid.zero {
    stroke: var(--border-strong);
  }
  .axis {
    font-family: var(--mono);
    font-size: 8px;
    fill: var(--text-2);
  }
  .line {
    fill: none;
    stroke: var(--acc);
    stroke-width: 2;
    stroke-linecap: round;
  }
  .hit {
    fill: transparent;
  }
  .dot {
    fill: var(--thumb);
    stroke: var(--acc);
    stroke-width: 2;
  }
  .pt {
    cursor: grab;
    outline: none;
  }
  .pt.drag {
    cursor: grabbing;
  }
  .pt:focus-visible .dot {
    stroke: var(--amber);
    stroke-width: 3;
  }
  .disabled {
    opacity: 0.45;
  }
  .disabled .pt {
    cursor: default;
  }
</style>
