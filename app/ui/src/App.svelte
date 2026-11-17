<script lang="ts">
  import { onMount } from 'svelte';
  import { app, type Tab } from './lib/store.svelte';
  import { outputName } from './lib/format';
  import Toggle from './components/Toggle.svelte';
  import ThemeButton from './components/ThemeButton.svelte';
  import EqTab from './panels/EqTab.svelte';
  import EffectsTab from './panels/EffectsTab.svelte';
  import MicTab from './panels/MicTab.svelte';
  import MixerTab from './panels/MixerTab.svelte';
  import DeviceTab from './panels/DeviceTab.svelte';
  import Disconnected from './panels/Disconnected.svelte';

  const st = $derived(app.state);
  const connected = $derived(st.connection === 'connected');
  // EQ, Effects and Mic are always shown; these tabs switch the fourth column. The Mixer tab exists
  // only where the OS offers the G6's audio controls (`mixer` is null otherwise).
  const TABS = $derived<{ id: Tab; label: string }[]>([
    ...(st.mixer ? [{ id: 'mix' as const, label: 'Mixer' }] : []),
    { id: 'out', label: 'Output' },
    { id: 'prof', label: 'Profiles' },
    { id: 'sys', label: 'System' },
  ]);
  // The G6 audio device can vanish while its tab is open.
  const tab = $derived<Tab>(app.tab === 'mix' && !st.mixer ? 'out' : app.tab);
  // Direct Mode owns the EQ and the effects.
  const dimmed = $derived(st.directMode);

  onMount(() => {
    app.start();
  });

</script>

<!-- The panel and its notice are shown before the first snapshot, so a failed first read is visible. -->
<div class="panel">
  {#if app.loaded}
    {#if !connected}
      <Disconnected />
    {:else}
      <div class="head">
        <div class="who">
          <span class="dot"></span>
          <div class="col">
            <span class="name">Sound BlasterX G6</span>
            <span class="sub">Connected · Firmware {st.firmware ?? '…'} · Profile: {st.activePreset ?? 'None'}</span>
          </div>
        </div>

        <div class="direct">
          <div class="col">
            <span class="n">Direct Mode</span>
            <span class="sub">
              {st.directMode
                ? 'EQ, SBX and Scout are off and come back when you turn it off.'
                : 'Bypass EQ, SBX and Scout. Mic untouched.'}
            </span>
          </div>
          <Toggle
            on={st.directMode}
            label="Direct Mode"
            onchange={(enabled) => app.run('set_direct_mode', { enabled })}
          />
        </div>

        <div class="right">
          {#if st.output}
            <div class="badge" role="status" aria-label="Active output: {outputName(st.output)}">
              {#if st.output === 'headphones'}
                <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 14v-2a9 9 0 0 1 18 0v2"></path><path d="M21 15a2 2 0 0 1-2 2h-1v-6h1a2 2 0 0 1 2 2z"></path><path d="M3 15a2 2 0 0 0 2 2h1v-6H5a2 2 0 0 0-2 2z"></path></svg>
              {:else}
                <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="2" width="14" height="20" rx="2"></rect><circle cx="12" cy="14" r="4"></circle><circle cx="12" cy="6" r="1"></circle></svg>
              {/if}
              <span>{outputName(st.output)}</span>
            </div>
          {/if}
          <ThemeButton />
        </div>
      </div>

      <div class="cols">
        <div class="pane scroll" class:dimmed inert={dimmed}><EqTab /></div>
        <div class="pane scroll" class:dimmed inert={dimmed}><EffectsTab /></div>
        <div class="pane scroll"><MicTab /></div>
        <div class="pane side">
          <div class="tabs" role="tablist" aria-label="Sections" style:grid-template-columns="repeat({TABS.length}, minmax(0, 1fr))">
            {#each TABS as t (t.id)}
              <button
                type="button"
                role="tab"
                aria-selected={tab === t.id}
                class:sel={tab === t.id}
                onclick={() => (app.tab = t.id)}
              >
                {t.label}
              </button>
            {/each}
          </div>
          <div class="scroll" role="tabpanel">
            {#if tab === 'mix'}
              <MixerTab />
            {:else}
              <DeviceTab section={tab} />
            {/if}
          </div>
        </div>
      </div>

    {/if}
  {/if}

  {#if app.toast}
    <div class="toast" role="alert">
      <span>{app.toast.message}</span>
      <button type="button" aria-label="Dismiss" onclick={() => app.dismissToast()}>×</button>
    </div>
  {/if}
</div>

<style>
  .panel {
    position: relative;
    width: 100%;
    height: 100vh;
    background: var(--bg);
    color: var(--text);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .head {
    padding: 10px 16px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    border-bottom: 1px solid var(--border);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    flex: none;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--acc);
    flex: none;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    font-size: 14px;
    letter-spacing: -0.01em;
  }
  .n {
    font-weight: 500;
  }
  .sub {
    color: var(--text-2);
    font-size: 11px;
  }
  .right {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  .badge {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0 12px;
    border-radius: 16px;
    border: 1px solid var(--border-2);
    background: var(--card);
    color: var(--text);
    font-size: 12px;
    font-weight: 500;
  }
  .direct {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 12px;
    border-radius: 12px;
    background: var(--card);
    border: 1px solid var(--border);
    min-width: 0;
  }
  .cols {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: 1.55fr 0.95fr 1.05fr 1.25fr;
    gap: 12px;
    padding: 10px 12px;
  }
  .pane {
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
  }
  .side {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .side > .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .tabs {
    display: grid;
    gap: 4px;
    padding: 4px;
    border-radius: 10px;
    background: var(--card);
    border: 1px solid var(--border);
  }
  .tabs button {
    height: 28px;
    border: none;
    border-radius: 7px;
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    background: transparent;
    color: var(--text-2);
  }
  .tabs button.sel {
    background: var(--border-2);
    color: var(--text);
  }
  .dimmed {
    opacity: 0.3;
  }
  .toast {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    width: min(520px, calc(100% - 24px));
    bottom: 12px;
    padding: 10px 12px;
    border-radius: 10px;
    border: 1px solid var(--amber);
    background: var(--toast-bg);
    color: var(--text);
    font-size: 12px;
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
  }
  .toast button {
    border: none;
    background: transparent;
    color: var(--text-2);
    font-size: 14px;
    line-height: 1;
    padding: 0 2px;
    cursor: pointer;
  }
</style>
