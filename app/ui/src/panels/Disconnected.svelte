<script lang="ts">
  import { app } from '../lib/store.svelte';
  import ThemeButton from '../components/ThemeButton.svelte';
</script>

<header>
  <span class="ring"></span>
  <div class="col">
    <span class="name">Sound BlasterX G6</span>
    <span class="sub">Not connected</span>
  </div>
  <span class="spacer"></span>
  <ThemeButton />
</header>
<div class="body">
  <svg
    aria-hidden="true"
    width="40"
    height="40"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    style="color: var(--text-2)"
    stroke-width="1.5"
    stroke-linecap="round"
    stroke-linejoin="round"
  >
    <path d="M10 17h4" /><path d="M12 7v10" /><path d="M8 7h8" /><path d="M9 3h6v4H9z" /><path d="M12 17v4" />
  </svg>
  <span class="t">Waiting for the G6</span>
  <span class="d">Plug it in over USB. Your last settings are put back as soon as it reconnects.</span>
</div>
<footer>
  <span>Reconnecting automatically</span>
  <span>Profile: {app.state.activePreset ?? 'None'}</span>
</footer>

<style>
  .spacer {
    flex: 1;
  }
  header {
    padding: 16px;
    display: flex;
    align-items: center;
    gap: 10px;
    border-bottom: 1px solid var(--border);
  }
  .ring {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    border: 2px solid var(--text-2);
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .name {
    font-weight: 600;
    font-size: 14px;
  }
  .sub {
    color: var(--text-2);
    font-size: 11px;
  }
  .body {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
    padding: 32px;
    text-align: center;
  }
  .t {
    font-weight: 600;
    font-size: 15px;
  }
  .d {
    color: var(--text-2);
    line-height: 1.5;
    max-width: 260px;
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
</style>
