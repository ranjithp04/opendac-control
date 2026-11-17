<script lang="ts">
  import { app } from '../lib/store.svelte';
  import { MAX_EQ_IMPORT_BYTES } from '../lib/api';
  import { BAND_LABELS, db, outputName } from '../lib/format';
  import Toggle from '../components/Toggle.svelte';
  import VSlider from '../components/VSlider.svelte';
  import SliderRow from '../components/SliderRow.svelte';
  import EqCurve from '../components/EqCurve.svelte';
  import {
    BUILTIN_CURVES,
    CUSTOM_NAME,
    MAX_CURVE_NAME,
    matches,
    suggestPreamp,
    type NamedCurve,
  } from '../lib/eqCurves';

  const st = $derived(app.state);
  const scout = $derived(st.scout && !st.directMode);
  // While Scout is on the device owns the curve; the snapshot carries it.
  const eqOn = $derived(scout || (st.eq.enabled && !st.directMode));
  // Gain being dragged per band (null when not dragging): label and curve follow the thumb.
  let liveBands = $state<(number | null)[]>(new Array(10).fill(null));
  // Same for the points of the response curve; the sliders follow those too.
  let curveBands = $state<(number | null)[]>(new Array(10).fill(null));
  const bands = $derived(st.eq.bands.map((v, i) => liveBands[i] ?? curveBands[i] ?? v));
  const out = $derived(outputName(st.output));
  const caption = $derived(scout ? 'Scout curve (set by the device)' : eqOn ? `${out} EQ` : `${out} EQ · off`);
  // Pre-amp being dragged (null when not dragging). Scout does not lock it (the device shows 0 dB then).
  let livePreamp = $state<number | null>(null);
  const preamp = $derived(livePreamp ?? st.eq.preamp);
  const curves = $derived<NamedCurve[]>([...BUILTIN_CURVES, ...st.eqCurves]);
  // First chip whose bands and pre-amp equal the active output's current values (built-ins first).
  // While Scout is on the bands are Scout's curve: no chip (not even Custom) is selected.
  const selected = $derived(scout ? null : curves.findIndex((c) => matches(c, { bands, preamp })));

  const suggested = $derived(suggestPreamp(bands));

  let saving = $state(false);
  let curveName = $state('');
  const canSave = $derived(curveName.trim().length > 0 && !scout);

  // Import from an AutoEq / Equalizer APO text file: the file is read here (webview file picker), mapped
  // by import_eq (no device access), then written like any curve with apply_eq_curve.
  let fileInput = $state<HTMLInputElement>();
  let importNotes = $state<string[]>([]);
  const canImport = $derived(!scout && !st.directMode);

  async function pickFile(e: Event & { currentTarget: HTMLInputElement }) {
    const file = e.currentTarget.files?.[0];
    e.currentTarget.value = ''; // the same file can be chosen again
    if (!file) return;
    importNotes = [];
    if (file.size > MAX_EQ_IMPORT_BYTES) {
      app.notify({ code: 'invalidValue', message: `EQ text is larger than ${MAX_EQ_IMPORT_BYTES / 1024} KiB` });
      return;
    }
    let text: string;
    try {
      text = await file.text();
    } catch {
      app.notify({ code: 'storage', message: `Could not read ${file.name}.` });
      return;
    }
    const eq = await app.run('import_eq', { text });
    if (!eq) return;
    if (await app.run('apply_eq_curve', { bands: eq.bands, preamp: eq.preamp })) {
      importNotes = [`Imported ${file.name}.`, ...eq.notes];
    }
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
    <EqCurve
      {bands}
      disabled={scout}
      bind:live={curveBands}
      oncommit={(band, gain) => app.run('set_eq_band', { band, gain })}
    />
    <div class="cap curvecap">
      <span>Approximate response: the G6's real filter shapes are not documented</span>
    </div>
    <div class="bands">
      {#each BAND_LABELS as l, i (l)}
        <div class="band">
          <span class="v">{db(bands[i])}</span>
          <VSlider
            value={curveBands[i] ?? st.eq.bands[i]}
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
        <div class="row">
          <button
            type="button"
            class="btn"
            disabled={st.directMode || scout || suggested === preamp}
            title="Set the pre-amp to cancel the largest band boost ({db(suggested)} dB)"
            onclick={() => app.run('set_eq_preamp', { db: suggested })}
          >
            Suggest pre-amp
          </button>
          <!-- Only a curve that matches no chip (Custom) is new enough to save. -->
          <button
            type="button"
            class="btn"
            disabled={selected !== -1}
            title={selected === -1 ? undefined : 'Change the EQ first: this curve is already a chip'}
            onclick={openSave}>Save curve…</button
          >
          <button
            type="button"
            class="btn"
            disabled={!canImport}
            title="Load an AutoEq / Equalizer APO text file (Preamp, Filter and GraphicEQ lines)"
            onclick={() => fileInput?.click()}>Import EQ…</button
          >
        </div>
      {/if}
      <input
        bind:this={fileInput}
        type="file"
        accept=".txt,text/plain"
        hidden
        aria-label="EQ text file"
        onchange={pickFile}
      />
    </div>
  </div>

  {#if importNotes.length}
    <div class="notes" role="status">
      <ul>
        {#each importNotes as n, i (i)}
          <li>{n}</li>
        {/each}
      </ul>
      <button type="button" class="close" aria-label="Dismiss notes" onclick={() => (importNotes = [])}>×</button>
    </div>
  {/if}
</div>

<style>
  .tab {
    display: flex;
    flex-direction: column;
    gap: 10px;
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
    /* Many saved curves scroll here instead of pushing the sliders down. */
    max-height: 62px;
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
  .curvecap {
    font-size: 10px;
    color: var(--text-2);
  }
  .preamp {
    padding: 8px 6px 2px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .preamp form,
  .row {
    display: flex;
    gap: 6px;
  }
  .row .btn {
    flex: 1;
    height: 28px;
    padding: 0 6px;
    font-size: 11px;
    white-space: nowrap;
  }
  .notes {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 10px;
    border-radius: 8px;
    border: 1px solid var(--border-2);
    font-size: 11px;
    color: var(--text-2);
  }
  .notes ul {
    margin: 0;
    padding-left: 14px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .close {
    border: none;
    background: transparent;
    color: var(--text-2);
    font-size: 14px;
    line-height: 1;
    padding: 0 2px;
    cursor: pointer;
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
