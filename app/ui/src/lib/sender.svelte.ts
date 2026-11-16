// Rate limiter for slider writes: at most one call every `interval` ms while
// dragging, one call in flight at a time, and the latest value is always sent
// (on release `flush()` sends it without waiting for the interval).

export class LatestSender<T> {
  /** True while a value is waiting or a call is in flight. */
  busy = $state(false);

  private latest: T | undefined;
  private has = false;
  private inflight = false;
  private urgent = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private lastStart = 0;

  constructor(
    private readonly send: (value: T) => Promise<unknown>,
    private readonly interval = 50,
  ) {}

  push(value: T): void {
    this.latest = value;
    this.has = true;
    this.busy = true;
    this.kick();
  }

  /** Send the pending value now (slider released). */
  flush(): void {
    if (!this.has) return;
    this.urgent = true;
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    if (!this.inflight) this.run();
  }

  private kick(): void {
    if (this.inflight || this.timer) return;
    const wait = this.urgent ? 0 : Math.max(0, this.lastStart + this.interval - Date.now());
    if (wait === 0) this.run();
    else this.timer = setTimeout(() => this.run(), wait);
  }

  private run(): void {
    this.timer = null;
    if (!this.has) {
      this.busy = this.inflight;
      return;
    }
    const value = this.latest as T;
    this.has = false;
    this.urgent = false;
    this.inflight = true;
    this.lastStart = Date.now();
    this.send(value)
      .catch(() => {})
      .finally(() => {
        this.inflight = false;
        if (this.has) this.kick();
        else this.busy = false;
      });
  }
}
