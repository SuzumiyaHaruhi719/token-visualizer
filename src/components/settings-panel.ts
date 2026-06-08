// Settings panel: a glass modal that consolidates every app toggle in one
// place (desktop pets, tray monitor, session-end sound + volume, and the
// optional Discord status config). Opened from the dashboard topbar gear.
//
// The panel fetches the live settings on open and writes each change back via
// the injected `updateSettings` (PUT /api/settings). Toggles persist instantly;
// the volume slider is debounced so dragging coalesces into one request.
// Designed to render entirely within view (the app hides scrollbars globally).

import type { AppSettings, AppSettingsPatch } from "../lib/types";

/** Injectable backend hooks (real api in the app; stubs in tests). */
export interface SettingsPanelDeps {
  getSettings: () => Promise<AppSettings>;
  updateSettings: (patch: AppSettingsPatch) => Promise<AppSettings>;
}

/** Debounce (ms) for the volume slider so a drag coalesces into one PUT. */
const VOLUME_DEBOUNCE_MS = 200;

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}

/** Volume float (0..1) -> percent integer (0..100) for the slider + label. */
function toPercent(volume: number): number {
  if (!Number.isFinite(volume)) return 0;
  return Math.round(Math.min(1, Math.max(0, volume)) * 100);
}

/** The panel markup. Controls are seeded from `s`; the sound slider is disabled
 *  when sound is off. */
function panelMarkup(s: AppSettings): string {
  const pct = toPercent(s.soundVolume);
  return `
    <div class="settings-overlay" data-settings-overlay>
      <div class="settings-card" role="dialog" aria-label="Settings">
        <div class="settings-head">
          <h2 class="settings-title">Settings</h2>
          <button class="settings-close" data-settings-close aria-label="Close settings">✕</button>
        </div>

        <div class="settings-rows">
          <label class="settings-row">
            <span class="settings-row-label">Desktop pets</span>
            <input class="settings-toggle" type="checkbox" data-field="petsEnabled" ${s.petsEnabled ? "checked" : ""} />
          </label>

          <label class="settings-row">
            <span class="settings-row-label">Tray monitor</span>
            <input class="settings-toggle" type="checkbox" data-field="monitorEnabled" ${s.monitorEnabled ? "checked" : ""} />
          </label>

          <label class="settings-row">
            <span class="settings-row-label">Session-end sound</span>
            <input class="settings-toggle" type="checkbox" data-field="soundEnabled" ${s.soundEnabled ? "checked" : ""} />
          </label>

          <div class="settings-row settings-row-slider">
            <span class="settings-row-label">Sound volume</span>
            <div class="settings-slider-wrap">
              <input class="settings-slider" type="range" min="0" max="100" step="1"
                     value="${pct}" data-field="soundVolume" ${s.soundEnabled ? "" : "disabled"} />
              <span class="settings-volume-value" data-volume-value>${pct}%</span>
            </div>
          </div>

          <label class="settings-row">
            <span class="settings-row-label">Discord status</span>
            <input class="settings-toggle" type="checkbox" data-field="discordEnabled" ${s.discordEnabled ? "checked" : ""} />
          </label>

          <div class="settings-row settings-row-input">
            <span class="settings-row-label">Discord client ID</span>
            <input class="settings-text" type="text" placeholder="application id"
                   value="${escapeHtml(s.discordClientId ?? "")}" data-field="discordClientId" />
          </div>
        </div>
      </div>
    </div>`;
}

/** A renderable settings panel handle: `open()` fetches + shows; `close()` hides. */
export interface SettingsPanel {
  open: () => Promise<void>;
  close: () => void;
}

/**
 * Mount a settings panel into `container`. The panel is hidden until `open()`.
 * Returns a handle with `open`/`close`. Tolerates a failed `getSettings` fetch
 * by falling back to all-on defaults so the panel still renders.
 */
export function createSettingsPanel(
  container: HTMLElement,
  deps: SettingsPanelDeps,
): SettingsPanel {
  let volumeTimer: ReturnType<typeof setTimeout> | null = null;
  let current: AppSettings | null = null;

  const fallback = (): AppSettings => ({
    petsEnabled: true,
    monitorEnabled: true,
    soundEnabled: true,
    soundVolume: 0.8,
    discordEnabled: false,
    discordClientId: null,
  });

  function clear(): void {
    if (volumeTimer !== null) {
      clearTimeout(volumeTimer);
      volumeTimer = null;
    }
    container.innerHTML = "";
  }

  function close(): void {
    clear();
  }

  /** Apply a patch and remember the freshest settings (best-effort). */
  async function apply(patch: AppSettingsPatch): Promise<void> {
    try {
      current = await deps.updateSettings(patch);
    } catch {
      // Keep the optimistic local value; never throw out of an input handler.
      if (current) current = { ...current, ...patch };
    }
  }

  /** Enable/disable the volume slider to mirror the sound toggle. */
  function syncSliderEnabled(): void {
    const slider = container.querySelector<HTMLInputElement>('[data-field="soundVolume"]');
    if (!slider) return;
    slider.disabled = !(current?.soundEnabled ?? true);
  }

  function wire(): void {
    const overlay = container.querySelector<HTMLElement>("[data-settings-overlay]");
    const closeBtn = container.querySelector<HTMLElement>("[data-settings-close]");

    closeBtn?.addEventListener("click", close);
    // Click-outside-to-close: only when the backdrop itself is clicked.
    overlay?.addEventListener("click", (e) => {
      if (e.target === overlay) close();
    });

    for (const box of container.querySelectorAll<HTMLInputElement>(".settings-toggle")) {
      box.addEventListener("change", () => {
        const field = box.dataset.field as keyof AppSettings | undefined;
        if (!field) return;
        void apply({ [field]: box.checked } as AppSettingsPatch).then(() => {
          if (field === "soundEnabled") syncSliderEnabled();
        });
      });
    }

    const slider = container.querySelector<HTMLInputElement>('[data-field="soundVolume"]');
    const valueLabel = container.querySelector<HTMLElement>("[data-volume-value]");
    slider?.addEventListener("input", () => {
      const pct = Number(slider.value);
      if (valueLabel) valueLabel.textContent = `${pct}%`;
      if (volumeTimer !== null) clearTimeout(volumeTimer);
      volumeTimer = setTimeout(() => {
        volumeTimer = null;
        void apply({ soundVolume: pct / 100 });
      }, VOLUME_DEBOUNCE_MS);
    });

    const text = container.querySelector<HTMLInputElement>('[data-field="discordClientId"]');
    text?.addEventListener("change", () => {
      void apply({ discordClientId: text.value.trim() });
    });
  }

  async function open(): Promise<void> {
    let settings: AppSettings;
    try {
      settings = await deps.getSettings();
    } catch {
      settings = fallback();
    }
    current = settings;
    container.innerHTML = panelMarkup(settings);
    wire();
  }

  return { open, close };
}
