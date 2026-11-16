<script lang="ts">
  import { onMount } from 'svelte';
  import { app, type Tab } from './lib/store.svelte';
  import { outputName } from './lib/format';
  import Toggle from './components/Toggle.svelte';
  import ThemeButton from './components/ThemeButton.svelte';
  import EqTab from './panels/EqTab.svelte';
  import EffectsTab from './panels/EffectsTab.svelte';
  import MicTab from './panels/MicTab.svelte';
  import DeviceTab from './panels/DeviceTab.svelte';
  import Disconnected from './panels/Disconnected.svelte';

  const st = $derived(app.state);
  const connected = $derived(st.connection === 'connected');
  const dimmed = $derived(st.directMode && (app.tab === 'eq' || app.tab === 'fx'));

  const TABS: { id: Tab; label: string }[] = [
    { id: 'eq', label: 'EQ' },
    { id: 'fx', label: 'Effects' },
    { id: 'mic', label: 'Mic' },
    { id: 'dev', label: 'Device' },
  ];

  onMount(() => {
    app.start();
  });

</script>

{#if app.loaded}
  <div class="panel">
    {#if !connected}
      <Disconnected />
    {:else}
      <div class="head">
        <div class="id">
          <div class="who">
            <span class="dot"></span>
            <div class="col">
              <span class="name">Sound BlasterX G6</span>
              <span class="sub">Connected · live</span>
            </div>
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

        <div class="direct">
          <div class="col">
            <span class="n">Direct Mode</span>
            <span class="sub">Bypass EQ, SBX and Scout. Mic untouched.</span>
          </div>
          <Toggle
            on={st.directMode}
            label="Direct Mode"
            onchange={(enabled) => app.run('set_direct_mode', { enabled })}
          />
        </div>

        <div class="tabs" role="tablist" aria-label="Sections">
          {#each TABS as t (t.id)}
            <button
              type="button"
              role="tab"
              aria-selected={app.tab === t.id}
              class:sel={app.tab === t.id}
              onclick={() => (app.tab = t.id)}
            >
              {t.label}
            </button>
          {/each}
        </div>
      </div>

      <div class="body scroll" role="tabpanel">
        {#if st.directMode && (app.tab === 'eq' || app.tab === 'fx')}
          <div class="banner">Direct Mode is on. EQ, SBX and Scout are off and come back when you turn it off.</div>
        {/if}
        <div class="content" class:dimmed inert={dimmed}>
          {#if app.tab === 'eq'}
            <EqTab />
          {:else if app.tab === 'fx'}
            <EffectsTab />
          {:else if app.tab === 'mic'}
            <MicTab />
          {:else}
            <DeviceTab />
          {/if}
        </div>
      </div>

      <footer>
        <span>Firmware {st.firmware ?? '…'}</span>
        <span>Profile: {st.activePreset ?? 'None'}</span>
      </footer>
    {/if}

    {#if app.toast}
      <div class="toast" role="alert">
        <span>{app.toast.message}</span>
        <button type="button" aria-label="Dismiss" onclick={() => app.dismissToast()}>×</button>
      </div>
    {/if}
  </div>
{/if}

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
    padding: 16px 16px 12px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    border-bottom: 1px solid var(--border);
  }
  .id {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .who {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
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
    padding: 10px 12px;
    border-radius: 12px;
    background: var(--card);
    border: 1px solid var(--border);
  }
  .tabs {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 4px;
    padding: 4px;
    border-radius: 10px;
    background: var(--card);
    border: 1px solid var(--border);
  }
  .tabs button {
    height: 30px;
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
  .body {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .banner {
    padding: 10px 12px;
    border-radius: 10px;
    border: 1px solid var(--border-2);
    color: var(--text-3);
    font-size: 12px;
  }
  .dimmed {
    opacity: 0.3;
  }
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 16px;
    border-top: 1px solid var(--border);
    color: var(--text-2);
    font-size: 11px;
  }
  .toast {
    position: absolute;
    left: 12px;
    right: 12px;
    bottom: 44px;
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
