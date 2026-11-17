<script lang="ts">
  import { app } from '../lib/store.svelte';
  import {
    call,
    MAX_PRESET_FILE_BYTES,
    toApiError,
    type DacFilter,
    type DeviceInfo,
    type DolbyMode,
    type Hotkeys,
    type Output,
  } from '../lib/api';
  import { acceleratorFromEvent, defaultAccelerator, META_LABEL } from '../lib/hotkeys';
  import { LatestSender } from '../lib/sender.svelte';
  import Toggle from '../components/Toggle.svelte';
  import Segmented from '../components/Segmented.svelte';

  /** Which part of the device settings the right-hand panel shows. One component, so form state survives a switch. */
  let { section }: { section: 'out' | 'prof' | 'sys' } = $props();

  const st = $derived(app.state);
  let newName = $state('');

  const FILTERS: { value: DacFilter; label: string }[] = [
    { value: 'fast-min', label: 'Fast roll-off · minimum phase' },
    { value: 'slow-min', label: 'Slow roll-off · minimum phase' },
    { value: 'nos', label: 'NOS · non-oversampling' },
    { value: 'fast-lin', label: 'Fast roll-off · linear phase' },
    { value: 'slow-lin', label: 'Slow roll-off · linear phase' },
  ];
  const DECODERS: { value: DolbyMode; label: string }[] = [
    { value: 'full', label: 'Full' },
    { value: 'normal', label: 'Normal' },
    { value: 'night', label: 'Night' },
  ];

  const OUTPUTS: { value: Output; label: string }[] = [
    { value: 'headphones', label: 'Headphones' },
    { value: 'speakers', label: 'Speakers' },
  ];
  const SWATCHES = ['ff0000', 'ff8000', 'ffd000', '00ff55', '00aaff', '6a5cff', 'ff3cc8', 'ffffff'];

  // LED colour. While the picker is in use (and until the writes have settled) the control shows
  // the value being dragged; snapshots do not overwrite it. The latest value is sent at most every
  // 50 ms, as for the sliders.
  const sender = new LatestSender<string>((color) => app.run('set_lighting_color', { color }));
  let dragColor = $state<string | null>(null);
  let settleTimer: ReturnType<typeof setTimeout> | null = null;
  const color = $derived(dragColor ?? st.lighting.color);

  function settle() {
    settleTimer = null;
    if (sender.busy) hold();
    else dragColor = null;
  }
  function hold() {
    if (settleTimer) clearTimeout(settleTimer);
    settleTimer = setTimeout(settle, 400);
  }
  function drag(hex: string) {
    dragColor = hex.replace('#', '').toLowerCase();
    sender.push(dragColor);
    hold();
  }
  function release() {
    sender.flush();
    hold();
  }

  // About this device. Read per connection by the backend (it does not change): fetched again when
  // the connection or the firmware shown in the snapshot changes. A failed read keeps what is shown.
  let info = $state<DeviceInfo | null>(null);
  let infoReq = 0;
  // Primitives, so the effect does not re-run for every snapshot (each one is a new object).
  const infoConn = $derived(st.connection);
  const infoFirmware = $derived(st.firmware);
  $effect(() => {
    void infoConn;
    void infoFirmware;
    const req = ++infoReq;
    void app.run('get_device_info').then((r) => {
      if (r && req === infoReq) info = r;
    });
  });
  const INFO_ROWS: { label: string; key: keyof DeviceInfo }[] = [
    { label: 'Firmware', key: 'firmware' },
    { label: 'Serial number', key: 'serial' },
    { label: 'DSP version', key: 'dspVersion' },
    { label: 'Hardware ID', key: 'hardwareId' },
  ];

  // Global hotkeys (opt-in). The backend validates, spells and registers them; a shortcut the OS
  // refuses shows its reason under the row.
  type HotkeyKey = 'toggleDirectMode' | 'nextProfile' | 'toggleSbx';
  const HOTKEYS: { key: HotkeyKey; label: string; fallback: string }[] = [
    { key: 'toggleDirectMode', label: 'Toggle Direct Mode', fallback: defaultAccelerator('D') },
    { key: 'nextProfile', label: 'Next profile', fallback: defaultAccelerator('P') },
    { key: 'toggleSbx', label: 'Toggle SBX', fallback: defaultAccelerator('S') },
  ];
  let capturing = $state<HotkeyKey | null>(null);

  /** Send the hotkey settings with `patch` applied; an empty accelerator means the default. */
  function saveHotkeys(patch: Partial<Record<HotkeyKey, string>> & { enabled?: boolean }) {
    const h: Hotkeys = st.hotkeys;
    return app.run('set_hotkeys', {
      enabled: patch.enabled ?? h.enabled,
      toggleDirectMode: patch.toggleDirectMode ?? h.toggleDirectMode.accelerator,
      nextProfile: patch.nextProfile ?? h.nextProfile.accelerator,
      toggleSbx: patch.toggleSbx ?? h.toggleSbx.accelerator,
    });
  }
  /**
   * Record a key combination in a field. Tab keeps moving focus; Escape leaves the field. A
   * recorded combination also leaves the field, so the stored accelerator is shown. Auto-repeat
   * is ignored, and an unchanged combination is not sent again (unless it failed to register).
   */
  function captureKey(key: HotkeyKey, e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
    if (e.key === 'Tab' && !e.ctrlKey && !e.altKey && !e.metaKey) return;
    const field = e.currentTarget;
    if (e.key === 'Escape') {
      field.blur();
      return;
    }
    e.preventDefault();
    if (e.repeat) return;
    const accelerator = acceleratorFromEvent(e);
    if (accelerator === null) return;
    const b = st.hotkeys[key];
    if (accelerator === b.accelerator && !b.error) {
      field.blur();
      return;
    }
    void saveHotkeys({ [key]: accelerator }).then((r) => {
      if (r && capturing === key) field.blur();
    });
  }

  // Presets hold EQ / effects / SBX, which Direct Mode owns.
  const presetsLocked = $derived(st.directMode);

  // Row being renamed / waiting for delete confirmation (one at a time).
  let renaming = $state<string | null>(null);
  let renameTo = $state('');
  let confirmDelete = $state<string | null>(null);

  function startRename(name: string) {
    confirmDelete = null;
    renaming = name;
    renameTo = name;
  }
  async function rename(from: string) {
    const to = renameTo.trim();
    if (!to || to === from) {
      renaming = null;
      return;
    }
    // On failure the field stays open and the store shows the reason.
    if ((await app.run('rename_preset', { from, to })) !== undefined) renaming = null;
  }
  async function remove(name: string) {
    confirmDelete = null;
    await app.run('delete_preset', { name });
  }
  function focusOnMount(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  // Auto-switch rule for an output. On failure the backend keeps the old rule, so the select goes back to it.
  async function setRule(output: Output, el: HTMLSelectElement) {
    const profile = el.value || null;
    if ((await app.run('set_auto_switch', { output, profile })) === undefined) {
      el.value = st.autoSwitch[output] ?? '';
    }
  }

  // Profile files. Reading uses the webview's file picker; writing is the native Save dialog (export_preset).
  let fileInput = $state<HTMLInputElement>();
  // A chosen file whose name was refused (invalid or already taken): ask for another name.
  let importing = $state<{ text: string; name: string } | null>(null);

  async function exportProfile(name: string) {
    const path = await app.run('export_preset', { name });
    if (path) app.say(`Exported “${name}” to ${path}`);
  }
  async function pickFile(e: Event & { currentTarget: HTMLInputElement }) {
    const file = e.currentTarget.files?.[0];
    e.currentTarget.value = ''; // the same file can be chosen again
    if (!file) return;
    if (file.size > MAX_PRESET_FILE_BYTES) {
      app.notify({ code: 'invalidValue', message: `invalid preset: profile file is larger than ${MAX_PRESET_FILE_BYTES / 1024} KiB` });
      return;
    }
    let text: string;
    try {
      text = await file.text();
    } catch {
      app.notify({ code: 'storage', message: `Could not read ${file.name}.` });
      return;
    }
    await importProfile(text, file.name.replace(/\.json$/i, '').trim());
  }
  async function importProfile(text: string, name: string) {
    try {
      app.apply(await call('import_preset', { text, name }));
      importing = null;
    } catch (e) {
      const err = toApiError(e);
      // A bad name can be fixed here; bad content cannot.
      importing = err.code === 'presetInvalidName' ? { text, name } : null;
      app.notify(err);
    }
  }

  async function save() {
    const name = newName.trim();
    if (!name) return;
    if ((await app.run('save_preset', { name })) !== undefined) newName = '';
  }
</script>

<div class="tab">
  {#if section === 'out'}
    <div class="card shaded" role="radiogroup" aria-label="DAC filter">
      <span class="h">DAC filter</span>
      {#each FILTERS as f (f.value)}
        {@const sel = st.dacFilter === f.value}
        <button
          type="button"
          role="radio"
          aria-checked={sel}
          class="filter"
          class:sel
          onclick={() => app.run('set_dac_filter', { filter: f.value })}
        >
          <span class="dot"></span>
          {f.label}
        </button>
      {/each}
    </div>

    <div class="card">
      <div class="col">
        <span class="n">Dolby Digital</span>
        <span class="sub">Dynamic range for the optical input</span>
      </div>
      <Segmented
        options={DECODERS}
        value={st.dolbyDecoder}
        label="Dolby Digital decoder"
        onpick={(mode) => app.run('set_dolby_decoder', { mode })}
      />
    </div>

    <div class="card">
      <div class="col">
        <span class="n">Output</span>
        <span class="sub">The G6 also switches automatically when headphones are plugged in.</span>
      </div>
      <Segmented
        options={OUTPUTS}
        value={st.output}
        label="Output"
        onpick={(output) => app.run('set_output', { output })}
      />
    </div>
  {:else if section === 'prof'}
    <div class="card">
      <span class="n">Profiles</span>
      <span class="sub">A profile saves all playback settings (EQ, pre-amp, SBX, Scout, effects) and applies them to the active output.</span>
      <div class="presets">
        {#each st.presets as name (name)}
          <div class="preset">
            {#if renaming === name}
              <form
                class="rename"
                onsubmit={(e) => {
                  e.preventDefault();
                  rename(name);
                }}
              >
                <input
                  type="text"
                  maxlength="64"
                  aria-label="New name for {name}"
                  bind:value={renameTo}
                  use:focusOnMount
                  onkeydown={(e) => e.key === 'Escape' && (renaming = null)}
                />
                <button type="submit" class="btn" disabled={!renameTo.trim()}>Save</button>
                <button type="button" class="btn" onclick={() => (renaming = null)}>Cancel</button>
              </form>
            {:else if confirmDelete === name}
              <span class="pname">Delete “{name}”?</span>
              <div class="actions">
                <button type="button" class="btn danger" onclick={() => remove(name)}>Delete</button>
                <button type="button" class="btn" onclick={() => (confirmDelete = null)}>Cancel</button>
              </div>
            {:else}
              <span class="pname">{name}</span>
              <div class="actions">
                <button
                  type="button"
                  class="icon"
                  aria-label="Rename {name}"
                  title="Rename"
                  onclick={() => startRename(name)}
                >
                  <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"></path><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"></path></svg>
                </button>
                <button
                  type="button"
                  class="icon"
                  aria-label="Export {name}"
                  title="Export to a file"
                  onclick={() => exportProfile(name)}
                >
                  <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v12"></path><path d="m7 10 5 5 5-5"></path><path d="M5 21h14"></path></svg>
                </button>
                <button
                  type="button"
                  class="icon"
                  aria-label="Delete {name}"
                  title="Delete"
                  onclick={() => {
                    renaming = null;
                    confirmDelete = name;
                  }}
                >
                  <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"></path><path d="M8 6V4h8v2"></path><path d="M19 6l-1 14H6L5 6"></path></svg>
                </button>
                <button
                  type="button"
                  class="btn"
                  aria-label="Apply {name}"
                  disabled={presetsLocked}
                  onclick={() => app.run('apply_preset', { name })}
                >
                  Apply
                </button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
      <form
        class="save"
        onsubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <label>
          Save current as
          <input type="text" maxlength="64" bind:value={newName} placeholder="Profile name" />
        </label>
        <button type="submit" class="primary" disabled={presetsLocked || !newName.trim()}>Save</button>
      </form>
      {#if presetsLocked}
        <span class="sub">Profiles are unavailable while Direct Mode is on.</span>
      {/if}
      {#if importing}
        <form
          class="save"
          onsubmit={(e) => {
            e.preventDefault();
            if (importing && importing.name.trim()) importProfile(importing.text, importing.name.trim());
          }}
        >
          <label>
            Name for the imported profile
            <input type="text" maxlength="64" bind:value={importing.name} use:focusOnMount />
          </label>
          <button type="submit" class="btn" disabled={!importing.name.trim()}>Import</button>
          <button type="button" class="btn" onclick={() => (importing = null)}>Cancel</button>
        </form>
      {:else}
        <div class="import">
          <button type="button" class="btn" onclick={() => fileInput?.click()}>Import profile…</button>
          <span class="sub">A .json file written by Export. Saved only; apply it from the list.</span>
        </div>
      {/if}
      <input
        bind:this={fileInput}
        type="file"
        accept=".json,application/json"
        hidden
        aria-label="Profile file"
        onchange={pickFile}
      />
      <div class="auto" role="group" aria-label="Switch profile automatically">
        <span class="n">Switch automatically</span>
        <span class="sub">Applies a profile when the G6 changes output. Skipped while Direct Mode is on.</span>
        {#each OUTPUTS as o (o.value)}
          <div class="rule">
            <span id="rule-{o.value}">When the output becomes {o.label}</span>
            <select
              aria-labelledby="rule-{o.value}"
              value={st.autoSwitch[o.value] ?? ''}
              onchange={(e) => setRule(o.value, e.currentTarget)}
            >
              <option value="">Do nothing</option>
              {#each st.presets as name (name)}
                <option value={name}>{name}</option>
              {/each}
            </select>
          </div>
        {/each}
      </div>
    </div>
  {:else}
    <div class="card">
      <div class="row">
        <span class="n">Lighting</span>
        <Toggle
          on={st.lighting.enabled}
          label="Lighting"
          onchange={(enabled) => app.run('set_lighting_enabled', { enabled })}
        />
      </div>
      <div class="row colors" class:dim={!st.lighting.enabled}>
        <input
          type="color"
          class="picker"
          aria-label="Lighting color"
          value={'#' + color}
          oninput={(e) => drag(e.currentTarget.value)}
          onchange={release}
        />
        <span class="hex" aria-hidden="true">#{color.toUpperCase()}</span>
        <div class="swatches" role="group" aria-label="Lighting color presets">
          {#each SWATCHES as sw (sw)}
            <button
              type="button"
              class="swatch"
              class:sel={color === sw}
              style:background="#{sw}"
              aria-label="Lighting color #{sw.toUpperCase()}"
              aria-pressed={color === sw}
              onclick={() => {
                drag(sw);
                release();
              }}
            ></button>
          {/each}
        </div>
      </div>
    </div>

    <div class="card login">
      <span class="n">Launch at login</span>
      <Toggle
        on={st.launchAtLogin}
        label="Launch at login"
        onchange={(enabled) => app.run('set_launch_at_login', { enabled })}
      />
    </div>

    <div class="card keys">
      <div class="row">
        <div class="col">
          <span class="n">Global shortcuts</span>
          <span class="sub">Work in any app while OpenDAC Control runs. Off by default.</span>
        </div>
        <Toggle
          on={st.hotkeys.enabled}
          label="Global shortcuts"
          onchange={(enabled) => saveHotkeys({ enabled })}
        />
      </div>
      {#each HOTKEYS as h (h.key)}
        {@const b = st.hotkeys[h.key]}
        <div class="key" class:off={!st.hotkeys.enabled}>
          <span id="hk-{h.key}">{h.label}</span>
          <input
            class="accel"
            readonly
            spellcheck="false"
            aria-labelledby="hk-{h.key}"
            aria-describedby={b.error ? `hk-err-${h.key}` : undefined}
            aria-invalid={b.error ? true : undefined}
            placeholder="Press keys…"
            value={capturing === h.key ? '' : b.accelerator}
            onfocus={() => (capturing = h.key)}
            onblur={() => (capturing = null)}
            onkeydown={(e) => captureKey(h.key, e)}
          />
          <button
            type="button"
            class="btn"
            disabled={b.accelerator === h.fallback}
            aria-label="Reset {h.label} to {h.fallback}"
            onclick={() => saveHotkeys({ [h.key]: '' })}>Reset</button>
        </div>
        {#if b.error}
          <p class="err" id="hk-err-{h.key}">{b.error}</p>
        {/if}
      {/each}
      <span class="sub">Click a field, then press the keys. Needs Ctrl, Alt or {META_LABEL} with a key.</span>
    </div>

    <div class="card about">
      <span class="n">About this device</span>
      <dl>
        {#each INFO_ROWS as r (r.key)}
          <dt>{r.label}</dt>
          <dd>{info?.[r.key] ?? '–'}</dd>
        {/each}
      </dl>
    </div>
  {/if}
</div>

<style>
  .tab {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: 12px;
    border: 1px solid var(--border);
  }
  .card.shaded {
    gap: 6px;
    background: var(--card);
  }
  .card.login {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
  }
  .key {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    font-size: 12px;
  }
  .key > span {
    flex: 1 1 auto;
    min-width: 0;
  }
  .key.off {
    opacity: 0.6;
  }
  .accel {
    flex: 1 1 96px;
    min-width: 0;
    font-family: var(--mono);
    text-align: center;
    cursor: pointer;
  }
  .accel:focus {
    border-color: var(--acc);
  }
  .err {
    margin: -4px 0 0;
    font-size: 11px;
    color: var(--danger);
  }
  .about dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 12px;
    margin: 0;
    font-size: 11px;
  }
  .about dt {
    color: var(--text-2);
  }
  .about dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
    font-family: var(--mono);
    text-align: right;
    user-select: text;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  .colors {
    justify-content: flex-start;
    flex-wrap: wrap;
  }
  .colors.dim {
    opacity: 0.5;
  }
  /* Native colour input, restyled to the theme: a round swatch with a ring. */
  .picker {
    flex: none;
    appearance: none;
    box-sizing: border-box;
    width: 32px;
    height: 32px;
    padding: 0;
    border-radius: 50%;
    border: 1px solid var(--border-strong);
    background: transparent;
    cursor: pointer;
    overflow: hidden;
  }
  .picker::-webkit-color-swatch-wrapper {
    padding: 0;
  }
  .picker::-webkit-color-swatch {
    border: none;
    border-radius: 50%;
  }
  .picker::-moz-color-swatch {
    border: none;
    border-radius: 50%;
  }
  .hex {
    flex: none;
    width: 62px;
    font-family: var(--mono);
    font-size: 11px;
    color: var(--text-2);
  }
  .swatches {
    flex: 1 1 auto;
    display: flex;
    justify-content: flex-start;
    gap: 6px;
  }
  .swatch {
    flex: none;
    box-sizing: border-box;
    width: 20px;
    height: 20px;
    padding: 0;
    border-radius: 50%;
    border: 1px solid var(--border-strong);
    cursor: pointer;
  }
  .swatch.sel {
    outline: 2px solid var(--acc);
    outline-offset: 2px;
  }
  .h {
    font-weight: 500;
    margin-bottom: 4px;
  }
  .n {
    font-weight: 500;
  }
  .sub {
    color: var(--text-2);
    font-size: 11px;
  }
  .filter {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 36px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: transparent;
    color: var(--text-3);
    font-size: 12px;
    text-align: left;
    cursor: pointer;
  }
  .filter.sel {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .dot {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid var(--off);
    background: transparent;
    flex: none;
  }
  .sel .dot {
    border-color: var(--acc);
    background: var(--acc);
  }
  .presets {
    /* A long list scrolls here instead of pushing the form off the panel. */
    max-height: 112px;
    overflow-y: auto;
  }
  .preset {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    min-height: 36px;
  }
  .pname {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
  }
  .icon {
    width: 30px;
    height: 30px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 8px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-2);
    cursor: pointer;
  }
  .icon:hover {
    color: var(--text);
    border-color: var(--border-2);
  }
  .rename {
    flex: 1;
    display: flex;
    gap: 6px;
  }
  .rename input {
    flex: 1;
    min-width: 0;
  }
  .danger {
    border-color: var(--danger);
    color: var(--danger);
  }
  .btn {
    height: 30px;
    padding: 0 12px;
    border-radius: 8px;
    border: 1px solid var(--border-2);
    background: transparent;
    color: var(--text);
    font-size: 12px;
    cursor: pointer;
  }
  .btn:disabled,
  .primary:disabled {
    opacity: 0.45;
  }
  .save {
    display: flex;
    gap: 8px;
    align-items: flex-end;
  }
  label {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    color: var(--text-2);
  }
  input {
    height: 32px;
    padding: 0 10px;
    border-radius: 8px;
    border: 1px solid var(--border-2);
    background: var(--card);
    color: var(--text);
    font-size: 12px;
  }
  .import {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .import .btn {
    flex: none;
    white-space: nowrap;
  }
  .auto {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 4px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }
  .rule {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    font-size: 12px;
  }
  select {
    flex: none;
    max-width: 50%;
    height: 32px;
    padding: 0 8px;
    border-radius: 8px;
    border: 1px solid var(--border-2);
    background: var(--card);
    color: var(--text);
    font-size: 12px;
  }
  .primary {
    height: 34px;
    padding: 0 14px;
    border-radius: 8px;
    border: none;
    background: var(--acc);
    color: var(--on-acc);
    font-weight: 600;
    font-size: 12px;
    cursor: pointer;
  }
</style>
