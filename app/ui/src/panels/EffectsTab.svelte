<script lang="ts">
  import { app } from '../lib/store.svelte';
  import type { EffectName } from '../lib/api';
  import Toggle from '../components/Toggle.svelte';
  import SliderRow from '../components/SliderRow.svelte';

  const st = $derived(app.state);
  const scout = $derived(st.scout && !st.directMode);
  const sbxOn = $derived(st.sbx && !scout && !st.directMode);

  const NAMES: [EffectName, string][] = [
    ['surround', 'Surround'],
    ['crystalizer', 'Crystalizer'],
    ['bass', 'Bass'],
    ['dialog', 'Dialog+'],
    ['smartVolume', 'Smart Volume'],
  ];

  const level = (v: number) => Math.round(v * 100);
</script>

<div class="tab">
  <div class="sbx">
    <div class="col">
      <span class="h">SBX Studio</span>
      <span class="sub">{scout ? 'Off while Scout Mode is on' : 'Surround, Crystalizer, Bass, Dialog+'}</span>
    </div>
    <Toggle on={sbxOn} label="SBX" onchange={(enabled) => app.run('set_sbx', { enabled })} />
  </div>

  {#each NAMES as [key, name] (key)}
    {@const e = st.effects[key]}
    {@const owned = key === 'smartVolume' && scout}
    <div class="fx" class:dim={key !== 'smartVolume' && !sbxOn}>
      <div class="line">
        <span class="n">{name}</span>
        <Toggle
          on={e.enabled}
          label={name}
          disabled={owned}
          onchange={(enabled) => app.run('set_effect_enabled', { effect: key, enabled })}
        />
      </div>
      <SliderRow
        value={level(e.level)}
        min={0}
        max={100}
        step={1}
        label="{name} level"
        format={(v) => `${v}%`}
        disabled={owned}
        oncommit={(v) => app.run('set_effect_level', { effect: key, level: v / 100 })}
      />
      {#if key === 'surround'}
        <SliderRow
          name="Distance"
          value={st.effects.surround.distance}
          min={10}
          max={300}
          step={0.5}
          label="Surround distance"
          format={String}
          oncommit={(distance) => app.run('set_surround_distance', { distance })}
        />
      {/if}
    </div>
  {/each}
</div>

<style>
  .tab {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .sbx {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px;
    border-radius: 12px;
    background: var(--card);
    border: 1px solid var(--border);
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .h {
    font-weight: 600;
  }
  .sub {
    color: var(--text-2);
    font-size: 11px;
  }
  .fx {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border-radius: 12px;
    border: 1px solid var(--border);
  }
  .fx.dim {
    opacity: 0.45;
  }
  .line {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .n {
    font-weight: 500;
  }
</style>
