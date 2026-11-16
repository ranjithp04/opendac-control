<script lang="ts">
  let {
    on,
    label,
    onchange,
    disabled = false,
    color,
  }: {
    on: boolean;
    label: string;
    onchange: (next: boolean) => void;
    disabled?: boolean;
    /** Track colour when on (default: accent). */
    color?: string;
  } = $props();
</script>

<button
  type="button"
  role="switch"
  aria-checked={on}
  aria-label={label}
  {disabled}
  class:on
  style:--track={color ?? 'var(--acc)'}
  onclick={() => onchange(!on)}
>
  <span></span>
</button>

<style>
  button {
    flex: none;
    width: 44px;
    height: 26px;
    padding: 3px;
    border-radius: 13px;
    border: none;
    cursor: pointer;
    display: flex;
    justify-content: flex-start;
    background: var(--rail);
  }
  button.on {
    justify-content: flex-end;
    background: var(--track);
  }
  button:disabled {
    opacity: 0.45;
  }
  span {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--text-2);
  }
  .on span {
    background: var(--on-acc);
  }
</style>
