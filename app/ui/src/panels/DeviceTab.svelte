<script lang="ts">
  import { app } from '../lib/store.svelte';
  import type { DacFilter, DolbyMode } from '../lib/api';
  import Toggle from '../components/Toggle.svelte';
  import Segmented from '../components/Segmented.svelte';

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

  async function save() {
    const name = newName.trim();
    if (!name) return;
    if ((await app.run('save_preset', { name })) !== undefined) newName = '';
  }
</script>

<div class="tab">
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
    <span class="n">Profiles</span>
    <span class="sub">A profile saves all playback settings (EQ, pre-amp, SBX, Scout, effects) and applies them to the active output.</span>
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
  </div>

  <div class="card login">
    <span class="n">Launch at login</span>
    <Toggle
      on={st.launchAtLogin}
      label="Launch at login"
      onchange={(enabled) => app.run('set_launch_at_login', { enabled })}
    />
  </div>
</div>

<style>
  .tab {
    display: flex;
    flex-direction: column;
    gap: 12px;
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
  .col {
    display: flex;
    flex-direction: column;
    gap: 2px;
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
