<script lang="ts">
  import { app } from '../lib/store.svelte';
  import type { MixerLevel, MixerScope } from '../lib/api';
  import Toggle from '../components/Toggle.svelte';
  import SliderRow from '../components/SliderRow.svelte';

  const mixer = $derived(app.state.mixer);
  const source = $derived(mixer?.recordingSource ?? null);

  const ROWS: { scope: MixerScope; name: string; sub: string }[] = [
    { scope: 'playback', name: 'Playback', sub: 'Speaker volume (the knob moves it too)' },
    { scope: 'monitoring', name: 'Monitoring', sub: 'Inputs heard through the output' },
    { scope: 'recording', name: 'Recording', sub: 'Level of the selected source' },
  ];

  const percent = (v: number) => Math.round(v * 100);
  const dbText = (l: MixerLevel) => (l.volumeDb === null ? '' : `${l.volumeDb.toFixed(1)} dB`);
</script>

<div class="tab">
  {#each ROWS as { scope, name, sub } (scope)}
    {@const l = mixer?.[scope] ?? null}
    {#if l}
      <div class="card" class:muted={l.mute}>
        <div class="line">
          <div class="col">
            <span class="n">{name}</span>
            <span class="sub">{sub}</span>
          </div>
          {#if l.mute !== null}
            <div class="mute">
              <span class="sub">Mute</span>
              <Toggle
                on={l.mute}
                label="{name} mute"
                onchange={(mute) => app.run('set_mixer_mute', { scope, mute })}
              />
            </div>
          {/if}
        </div>
        {#if scope === 'recording' && source}
          <div class="chips" role="group" aria-label="Recording source">
            {#each source.options as o (o.id)}
              <button
                type="button"
                class="chip"
                class:sel={o.id === source.current}
                aria-pressed={o.id === source.current}
                onclick={() => app.run('set_mixer_source', { id: o.id })}
              >
                {o.name}
              </button>
            {/each}
          </div>
        {/if}
        <SliderRow
          value={percent(l.volume)}
          min={0}
          max={100}
          step={1}
          label="{name} volume"
          format={(v) => `${v}%`}
          oncommit={(v) => app.run('set_mixer_volume', { scope, volume: v / 100 })}
        />
        {#if dbText(l)}<span class="db">{dbText(l)}</span>{/if}
      </div>
    {/if}
  {/each}
  <p class="note">
    The G6's USB audio controls, the same ones Audio MIDI Setup shows (not the G6's own protocol).
    macOS shows one recording and one monitoring level, not one per input.
  </p>
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
    gap: 10px;
    padding: 12px;
    border-radius: 12px;
    border: 1px solid var(--border);
  }
  .card.muted {
    opacity: 0.6;
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
  .mute {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .n {
    font-weight: 500;
  }
  .sub {
    color: var(--text-2);
    font-size: 11px;
  }
  .db {
    align-self: flex-end;
    font-family: var(--mono);
    font-size: 11px;
    color: var(--text-2);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
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
  .note {
    margin: 0;
    color: var(--text-2);
    font-size: 11px;
    line-height: 1.4;
  }
</style>
