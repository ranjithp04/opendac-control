<script lang="ts">
  import { app } from '../lib/store.svelte';
  import { db, outputName } from '../lib/format';
  import type { MicBoost } from '../lib/api';
  import Toggle from '../components/Toggle.svelte';
  import Segmented from '../components/Segmented.svelte';
  import SliderRow from '../components/SliderRow.svelte';
  import VSlider from '../components/VSlider.svelte';
  import {
    MAX_CURVE_NAME,
    MIC_BAND_LABELS,
    MIC_BUILTIN_CURVES,
    MIC_CUSTOM_NAME,
    micMatches,
    type NamedMicCurve,
  } from '../lib/micEqCurves';

  const st = $derived(app.state);
  const m = $derived(st.mic);
  const nr = $derived(Math.round(m.noiseReduction * 100));

  // Gain being dragged per band (null when not dragging): the label follows the thumb.
  let liveBands = $state<(number | null)[]>(new Array(8).fill(null));
  const bands = $derived(m.eqBands.map((v, i) => liveBands[i] ?? v));
  const curves = $derived<NamedMicCurve[]>([...MIC_BUILTIN_CURVES, ...st.micEqCurves]);
  // First chip whose bands equal the active output's current values (presets first).
  const selected = $derived(curves.findIndex((c) => micMatches(c, { bands })));

  let saving = $state(false);
  let curveName = $state('');
  const canSave = $derived(curveName.trim().length > 0);

  // On failure the store shows the reason and the select goes back to the current curve.
  async function pick(el: HTMLSelectElement) {
    const c = curves[Number(el.value)];
    if (!c) return;
    if ((await app.run('apply_mic_eq_curve', { bands: c.bands })) === undefined) el.value = String(selected);
  }
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
    if (await app.run('save_mic_eq_curve', { name: curveName.trim() })) saving = false;
  }

  const BOOSTS: { value: MicBoost; label: string }[] = [0, 10, 20, 30].map((v) => ({
    value: v as MicBoost,
    label: `${v} dB`,
  }));
</script>

<div class="tab">
  <div class="card shaded boost">
    <div class="line">
      <span class="n">Mic boost</span>
      <!-- Mic settings are saved per output. -->
      <span class="sub">{st.output ? `${outputName(st.output)} values` : 'Saved per output'}</span>
    </div>
    <Segmented
      options={BOOSTS}
      value={m.boost}
      label="Mic boost"
      mono
      trackBg="var(--bg)"
      onpick={(db) => app.run('set_mic_boost', { db })}
    />
  </div>

  <div class="card">
    <div class="line">
      <div class="col">
        <span class="n">Voice Clarity</span>
        <span class="sub">Noise reduction for your voice</span>
      </div>
      <Toggle
        on={m.voiceClarity}
        label="Voice Clarity"
        onchange={(enabled) => app.run('set_voice_clarity', { enabled })}
      />
    </div>
    {#if m.voiceClarity}
      <SliderRow
        name="Noise red."
        value={nr}
        min={0}
        max={100}
        step={2}
        label="Noise reduction level"
        format={(v) => `${v}%`}
        oncommit={(v) => app.run('set_noise_reduction', { level: v / 100 })}
      />
    {/if}
  </div>

  <div class="card">
    <div class="line">
      <div class="col">
        <span class="n">Mic EQ</span>
        <span class="sub">8-band EQ set on the device</span>
      </div>
      <Toggle on={m.eqEnabled} label="Mic EQ" onchange={(enabled) => app.run('set_mic_eq_enabled', { enabled })} />
    </div>
    <div class="pick">
      <select aria-label="Mic EQ preset" value={String(selected)} onchange={(e) => pick(e.currentTarget)}>
        {#if selected === -1}
          <option value="-1" disabled>{MIC_CUSTOM_NAME}</option>
        {/if}
        {#each curves as c, i (c.name)}
          <option value={String(i)}>{c.name}</option>
        {/each}
      </select>
      {#if selected >= MIC_BUILTIN_CURVES.length}
        <button
          type="button"
          class="btn"
          aria-label="Delete curve {curves[selected].name}"
          onclick={() => app.run('delete_mic_eq_curve', { name: curves[selected].name })}
        >
          Delete
        </button>
      {/if}
    </div>
    <div class="bands">
      {#each MIC_BAND_LABELS as l, i (l)}
        <div class="band">
          <span class="v">{db(bands[i])}</span>
          <VSlider
            value={m.eqBands[i]}
            min={-12}
            max={12}
            step={0.5}
            height={84}
            label="Mic EQ band {l}"
            bind:live={liveBands[i]}
            oncommit={(gain) => app.run('set_mic_eq_band', { band: i, gain })}
          />
          <span class="l">{l}</span>
        </div>
      {/each}
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
        <div class="savebar">
          <!-- Only a curve that matches no chip (Custom) is new enough to save. -->
          <button
            type="button"
            class="btn"
            disabled={selected !== -1}
            title={selected === -1 ? undefined : 'Change the mic EQ first: this curve is already a chip'}
            onclick={openSave}>Save curve…</button
          >
          <span class="sub">Names and frequencies are placeholders.</span>
        </div>
      {/if}
    </div>
  </div>

  <div class="card">
    <div class="line">
      <div class="col">
        <span class="n">Echo cancellation</span>
        <span class="sub">Removes speaker echo</span>
      </div>
      <Toggle on={m.aec} label="Echo cancellation" onchange={(enabled) => app.run('set_mic_aec', { enabled })} />
    </div>
    <div class="line">
      <div class="col">
        <span class="n">Recording Smart Volume</span>
        <span class="sub">Evens out mic level</span>
      </div>
      <Toggle
        on={m.smartVolume}
        label="Recording Smart Volume"
        onchange={(enabled) => app.run('set_mic_smart_volume', { enabled })}
      />
    </div>
  </div>
</div>

<style>
  .tab {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border-radius: 12px;
    border: 1px solid var(--border);
  }
  .card.shaded {
    gap: 8px;
    background: var(--card);
  }

  .line {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .n {
    font-weight: 500;
  }
  .sub {
    color: var(--text-2);
    font-size: 11px;
  }
  .pick {
    display: flex;
    gap: 6px;
  }
  select {
    flex: 1;
    min-width: 0;
    height: 30px;
    padding: 0 8px;
    border-radius: 8px;
    border: 1px solid var(--border-2);
    background: var(--card);
    color: var(--text);
    font-size: 12px;
  }
  .bands {
    display: grid;
    grid-template-columns: repeat(8, minmax(0, 1fr));
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
  .savebar {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .savebar .btn {
    flex: none;
    white-space: nowrap;
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
