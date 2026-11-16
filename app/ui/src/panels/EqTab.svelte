<script lang="ts">
  import { app } from '../lib/store.svelte';
  import { BAND_LABELS, db, outputName } from '../lib/format';
  import Toggle from '../components/Toggle.svelte';
  import VSlider from '../components/VSlider.svelte';
  import SliderRow from '../components/SliderRow.svelte';
  import { BUILTIN_CURVES, CUSTOM_NAME, MAX_CURVE_NAME, matches, type NamedCurve } from '../lib/eqCurves';

  const st = $derived(app.state);
  const scout = $derived(st.scout && !st.directMode);
  // While Scout is on the device owns the curve; the snapshot carries it.
  const eqOn = $derived(scout || (st.eq.enabled && !st.directMode));
  // Gain being dragged per band (null when not dragging): label and curve follow the thumb.
  let liveBands = $state<(number | null)[]>(new Array(10).fill(null));
  const bands = $derived(st.eq.bands.map((v, i) => liveBands[i] ?? v));
  const out = $derived(outputName(st.output));
  const caption = $derived(scout ? 'Scout curve (set by the device)' : eqOn ? `${out} EQ` : `${out} EQ · off`);
  // Pre-amp being dragged (null when not dragging); not Scout-owned, so it stays editable in Scout.
  let livePreamp = $state<number | null>(null);
  const preamp = $derived(livePreamp ?? st.eq.preamp);
  const curves = $derived<NamedCurve[]>([...BUILTIN_CURVES, ...st.eqCurves]);
  // First chip whose bands and pre-amp equal the active output's current values (built-ins first).
  // While Scout is on the bands are Scout's curve: no chip (not even Custom) is selected.
  const selected = $derived(scout ? null : curves.findIndex((c) => matches(c, { bands, preamp })));

  let saving = $state(false);
  let curveName = $state('');
  const canSave = $derived(curveName.trim().length > 0 && !scout);

  function focusOnMount(node: HTMLElement) {
    node.focus();
  }
  function openSave() {
    curveName = '';
    saving = true;
  }
  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!canSave) return;
    // On failure the form stays open and the store shows the reason.
    if (await app.run('save_eq_curve', { name: curveName.trim() })) saving = false;
  }
</script>

<div class="tab">
  <div class="top">
    <div class="eq">
      <Toggle
        on={eqOn}
        label="Equalizer"
        disabled={scout}
        onchange={(enabled) => app.run('set_eq_enabled', { enabled })}
      />
      <span class="title">Equalizer</span>
    </div>
    <button
      type="button"
      class="scout"
      class:on={scout}
      aria-pressed={scout}
      onclick={() => app.run('set_scout', { enabled: !scout })}
    >
      Scout Mode
    </button>
  </div>

  <div class="card">
    <div class="chips" role="group" aria-label="EQ curves">
      {#each curves as c, i (c.name)}
        {@const user = i >= BUILTIN_CURVES.length}
        <span class="chipwrap" class:user>
          <button
            type="button"
            class="chip"
            class:sel={i === selected}
            aria-pressed={i === selected}
            disabled={scout}
            onclick={() => app.run('apply_eq_curve', { bands: c.bands, preamp: c.preamp })}
          >
            {c.name}
          </button>
          {#if user}
            <button
              type="button"
              class="del"
              aria-label="Delete curve {c.name}"
              onclick={() => app.run('delete_eq_curve', { name: c.name })}
            >
              ×
            </button>
          {/if}
        </span>
      {/each}
      {#if selected === -1}
        <span class="chip sel custom" aria-current="true">{CUSTOM_NAME}</span>
      {/if}
    </div>
    <div class="cap">
      <span>{caption}</span>
      <span class="mono">±12 dB</span>
    </div>
    <div class="bands">
      {#each BAND_LABELS as l, i (l)}
        <div class="band">
          <span class="v">{db(bands[i])}</span>
          <VSlider
            value={st.eq.bands[i]}
            min={-12}
            max={12}
            step={0.5}
            label="{l} Hz"
            bind:live={liveBands[i]}
            disabled={scout}
            oncommit={(gain) => app.run('set_eq_band', { band: i, gain })}
          />
          <span class="l">{l}</span>
        </div>
      {/each}
    </div>
    <div class="preamp">
      <SliderRow
        name="Pre-amp"
        value={st.eq.preamp}
        min={-6}
        max={6}
        step={0.5}
        label="Pre-amp"
        format={(v) => `${db(v)} dB`}
        valueWidth={54}
        bind:live={livePreamp}
        disabled={st.directMode}
        oncommit={(v) => app.run('set_eq_preamp', { db: v })}
      />
    </div>
  </div>

  <div class="save">
    {#if saving}
      <form onsubmit={save}>
        <input
          type="text"
          class="name"
          aria-label="Curve name"
          placeholder="Curve name"
          maxlength={MAX_CURVE_NAME}
          autocomplete="off"
          spellcheck="false"
          bind:value={curveName}
          use:focusOnMount
          onkeydown={(e) => e.key === 'Escape' && (saving = false)}
        />
        <button type="submit" class="btn primary" disabled={!canSave}>Save</button>
        <button type="button" class="btn" onclick={() => (saving = false)}>Cancel</button>
      </form>
    {:else}
      <!-- Only a curve that matches no chip (Custom) is new enough to save. -->
      <button
        type="button"
        class="btn"
        disabled={selected !== -1}
        title={selected === -1 ? undefined : 'Change the EQ first: this curve is already a chip'}
        onclick={openSave}>Save curve…</button
      >
    {/if}
  </div>
</div>

<style>
  .tab {
    display: flex;
    flex-direction: column;
    gap: 16px;
    /* WebKit drops the scroll container's bottom padding: keep the save form off the footer. */
    padding-bottom: 16px;
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .eq {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .title {
    font-weight: 500;
  }
  .scout {
    height: 30px;
    padding: 0 12px;
    border-radius: 15px;
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    border: 1px solid var(--border-2);
    background: transparent;
    color: var(--text-3);
  }
  .scout.on {
    border-color: var(--amber);
    background: var(--amber);
    color: var(--on-amber);
  }
  .card {
    border-radius: 12px;
    background: var(--card);
    border: 1px solid var(--border);
    padding: 12px 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .cap {
    display: flex;
    justify-content: space-between;
    padding: 0 6px;
    font-size: 11px;
    color: var(--text-2);
  }
  .mono {
    font-family: var(--mono);
    font-weight: 500;
  }
  .bands {
    display: grid;
    grid-template-columns: repeat(10, minmax(0, 1fr));
    justify-items: center;
  }
  .band {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }
  .v {
    font-family: var(--mono);
    font-weight: 500;
    font-size: 10px;
    color: var(--text);
  }
  .l {
    font-size: 10px;
    color: var(--text-2);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 0 6px 4px;
  }
  .chipwrap {
    display: inline-flex;
    align-items: stretch;
  }
  .chip {
    height: 26px;
    padding: 0 10px;
    display: inline-flex;
    align-items: center;
    border-radius: 13px;
    border: 1px solid var(--border-2);
    background: transparent;
    color: var(--text-3);
    font-size: 11px;
    font-weight: 500;
    cursor: pointer;
    white-space: nowrap;
  }
  .chip.sel {
    border-color: var(--acc);
    background: var(--border-2);
    color: var(--text);
  }
  .chip.custom {
    cursor: default;
  }
  .chip:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .user .chip {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
    border-right: none;
    padding-right: 6px;
  }
  .del {
    width: 22px;
    border: 1px solid var(--border-2);
    border-left: none;
    border-radius: 0 13px 13px 0;
    background: transparent;
    color: var(--text-2);
    font-size: 13px;
    line-height: 1;
    padding: 0 2px 0 0;
    cursor: pointer;
  }
  .del:hover {
    color: var(--text);
  }
  .user:has(.chip.sel) .del {
    border-color: var(--acc);
    background: var(--border-2);
  }
  .preamp {
    padding: 8px 6px 2px;
  }
  .save form {
    display: flex;
    gap: 8px;
  }
  .name {
    flex: 1;
    min-width: 0;
    height: 32px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--border-2);
    background: var(--card);
    color: var(--text);
    font-size: 12px;
  }
  .btn {
    height: 32px;
    padding: 0 12px;
    border-radius: 8px;
    border: 1px solid var(--border-2);
    background: transparent;
    color: var(--text-3);
    font-size: 12px;
    cursor: pointer;
  }
  .btn.primary {
    border-color: var(--acc);
    color: var(--text);
  }
  .btn:disabled {
    opacity: 0.45;
  }
</style>
