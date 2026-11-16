<script lang="ts">
  import { app } from '../lib/store.svelte';
  import { outputName } from '../lib/format';
  import type { MicBoost } from '../lib/api';
  import Toggle from '../components/Toggle.svelte';
  import Segmented from '../components/Segmented.svelte';
  import SliderRow from '../components/SliderRow.svelte';

  const st = $derived(app.state);
  const m = $derived(st.mic);
  const nr = $derived(Math.round(m.noiseReduction * 100));

  const BOOSTS: { value: MicBoost; label: string }[] = [0, 10, 20, 30].map((v) => ({
    value: v as MicBoost,
    label: `${v} dB`,
  }));
</script>

<div class="tab">
  <div class="card shaded">
    <span class="n">Mic boost</span>
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
  </div>

  <div class="card">
    <div class="line">
      <div class="col">
        <span class="n">Echo cancellation</span>
        <span class="sub">Removes speaker echo from the mic</span>
      </div>
      <Toggle on={m.aec} label="Echo cancellation" onchange={(enabled) => app.run('set_mic_aec', { enabled })} />
    </div>
  </div>

  <div class="card">
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

  <span class="note">
    {#if st.output}
      Mic settings are saved per output: these are the {outputName(st.output).toLowerCase()} values.
    {:else}
      Mic settings are saved per output.
    {/if}
  </span>
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
  .note {
    color: var(--text-2);
    font-size: 11px;
    padding: 0 4px;
  }
</style>
