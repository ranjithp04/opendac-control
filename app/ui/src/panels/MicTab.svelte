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
    <div class="chips" role="group" aria-label="Mic EQ presets">
      {#each curves as c, i (c.name)}
        {@const user = i >= MIC_BUILTIN_CURVES.length}
        <span class="chipwrap" class:user>
          <button
            type="button"
            class="chip"
            class:sel={i === selected}
            aria-pressed={i === selected}
            onclick={() => app.run('apply_mic_eq_curve', { bands: c.bands })}
          >
            {c.name}
          </button>
          {#if user}
            <button
              type="button"
              class="del"
              aria-label="Delete curve {c.name}"
              onclick={() => app.run('delete_mic_eq_curve', { name: c.name })}
            >
              ×
            </button>
          {/if}
        </span>
      {/each}
      {#if selected === -1}
        <span class="chip sel custom" aria-current="true">{MIC_CUSTOM_NAME}</span>
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
            height={64}
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
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    /* Many saved curves scroll here instead of pushing the sliders down. */
    max-height: 58px;
    overflow-y: auto;
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
