import { describe, it, expect, vi, beforeEach } from "vitest";

import { createSettingsPanel } from "./settings-panel";
import type { AppSettings, AppSettingsPatch } from "../lib/types";

function settings(overrides: Partial<AppSettings> = {}): AppSettings {
  return {
    petsEnabled: true,
    monitorEnabled: true,
    soundEnabled: true,
    soundVolume: 0.8,
    discordEnabled: false,
    discordClientId: null,
    ...overrides,
  };
}

function mount(): HTMLElement {
  const host = document.createElement("div");
  document.body.appendChild(host);
  return host;
}

beforeEach(() => {
  document.body.innerHTML = "";
  vi.useRealTimers();
});

describe("createSettingsPanel", () => {
  it("renders every control seeded from the fetched settings", async () => {
    const host = mount();
    const deps = {
      getSettings: vi.fn(async () => settings({ soundVolume: 0.5, soundEnabled: true })),
      updateSettings: vi.fn(async (p: AppSettingsPatch) => settings(p)),
    };
    const panel = createSettingsPanel(host, deps);
    await panel.open();

    expect(host.querySelector('[data-field="petsEnabled"]')).toBeTruthy();
    expect(host.querySelector('[data-field="monitorEnabled"]')).toBeTruthy();
    expect(host.querySelector('[data-field="soundEnabled"]')).toBeTruthy();
    const slider = host.querySelector<HTMLInputElement>('[data-field="soundVolume"]');
    expect(slider?.value).toBe("50");
    expect(host.querySelector("[data-volume-value]")?.textContent).toBe("50%");
    expect(host.querySelector('[data-field="discordEnabled"]')).toBeTruthy();
    expect(host.querySelector('[data-field="discordClientId"]')).toBeTruthy();
  });

  it("calls updateSettings when a toggle is flipped", async () => {
    const host = mount();
    const updateSettings = vi.fn(async (p: AppSettingsPatch) => settings(p));
    const panel = createSettingsPanel(host, {
      getSettings: vi.fn(async () => settings()),
      updateSettings,
    });
    await panel.open();

    const pets = host.querySelector<HTMLInputElement>('[data-field="petsEnabled"]')!;
    pets.checked = false;
    pets.dispatchEvent(new Event("change"));

    expect(updateSettings).toHaveBeenCalledWith({ petsEnabled: false });
  });

  it("debounces the volume slider into a single update", async () => {
    vi.useFakeTimers();
    const host = mount();
    const updateSettings = vi.fn(async (p: AppSettingsPatch) => settings(p));
    const panel = createSettingsPanel(host, {
      getSettings: vi.fn(async () => settings()),
      updateSettings,
    });
    await panel.open();

    const slider = host.querySelector<HTMLInputElement>('[data-field="soundVolume"]')!;
    slider.value = "30";
    slider.dispatchEvent(new Event("input"));
    slider.value = "60";
    slider.dispatchEvent(new Event("input"));
    // Label updates live even before the debounce fires.
    expect(host.querySelector("[data-volume-value]")?.textContent).toBe("60%");
    expect(updateSettings).not.toHaveBeenCalled();

    vi.advanceTimersByTime(200);
    expect(updateSettings).toHaveBeenCalledTimes(1);
    expect(updateSettings).toHaveBeenCalledWith({ soundVolume: 0.6 });
    vi.useRealTimers();
  });

  it("disables the volume slider when sound is off", async () => {
    const host = mount();
    const panel = createSettingsPanel(host, {
      getSettings: vi.fn(async () => settings({ soundEnabled: false })),
      updateSettings: vi.fn(async (p: AppSettingsPatch) => settings(p)),
    });
    await panel.open();

    const slider = host.querySelector<HTMLInputElement>('[data-field="soundVolume"]')!;
    expect(slider.disabled).toBe(true);
  });

  it("re-enables the slider when the sound toggle is turned on", async () => {
    const host = mount();
    const panel = createSettingsPanel(host, {
      getSettings: vi.fn(async () => settings({ soundEnabled: false })),
      updateSettings: vi.fn(async (p: AppSettingsPatch) =>
        settings({ soundEnabled: false, ...p }),
      ),
    });
    await panel.open();

    const sound = host.querySelector<HTMLInputElement>('[data-field="soundEnabled"]')!;
    sound.checked = true;
    sound.dispatchEvent(new Event("change"));
    // Let the async apply() resolve so syncSliderEnabled runs.
    await Promise.resolve();
    await Promise.resolve();

    const slider = host.querySelector<HTMLInputElement>('[data-field="soundVolume"]')!;
    expect(slider.disabled).toBe(false);
  });

  it("renders defaults when the settings fetch fails", async () => {
    const host = mount();
    const panel = createSettingsPanel(host, {
      getSettings: vi.fn(async () => {
        throw new Error("network down");
      }),
      updateSettings: vi.fn(async (p: AppSettingsPatch) => settings(p)),
    });
    await panel.open(); // must not throw

    // Falls back to all-on defaults (80% volume).
    const slider = host.querySelector<HTMLInputElement>('[data-field="soundVolume"]');
    expect(slider?.value).toBe("80");
    expect(
      host.querySelector<HTMLInputElement>('[data-field="petsEnabled"]')?.checked,
    ).toBe(true);
  });

  it("closes on the close button and on backdrop click", async () => {
    const host = mount();
    const panel = createSettingsPanel(host, {
      getSettings: vi.fn(async () => settings()),
      updateSettings: vi.fn(async (p: AppSettingsPatch) => settings(p)),
    });

    await panel.open();
    expect(host.querySelector("[data-settings-overlay]")).toBeTruthy();
    host.querySelector<HTMLElement>("[data-settings-close]")!.click();
    expect(host.querySelector("[data-settings-overlay]")).toBeNull();

    await panel.open();
    const overlay = host.querySelector<HTMLElement>("[data-settings-overlay]")!;
    overlay.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    // Target is the overlay itself -> closes.
    expect(host.querySelector("[data-settings-overlay]")).toBeNull();
  });
});
