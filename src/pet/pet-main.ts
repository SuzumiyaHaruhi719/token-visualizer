// Clawd pet bootstrap: reads ?session=<id>, subscribes to SSE `sessions`,
// finds its session, maps PetState -> animation class, renders project label,
// tool tag, and per-state bubble. Click -> open dashboard.

import "../pet/pet.css";
import { buildClawdSvg, stateToClass } from "./Clawd";
import { subscribe } from "../lib/api";
import { baseUrl } from "../lib/api";
import type { PetState, SessionState, CmServerEvent } from "../lib/types";

/** Per-state speech bubble glyph (empty = no bubble). */
const BUBBLE: Record<PetState["kind"], string> = {
  idle: "",
  thinking: "💭",
  working: "",
  responding: "···",
  waiting: "👀",
  sleeping: "💤",
};

function getSessionId(): string | null {
  const params = new URLSearchParams(
    typeof location !== "undefined" ? location.search : "",
  );
  return params.get("session");
}

interface PetEls {
  stage: HTMLElement;
  bubble: HTMLElement;
  toolTag: HTMLElement;
  label: HTMLElement;
}

function renderShell(root: HTMLElement): PetEls {
  root.classList.add("pet-root");
  root.setAttribute("data-tauri-drag-region", "");
  root.innerHTML = `
    <div class="pet-bubble" id="pet-bubble" data-tauri-drag-region></div>
    <div class="pet-stage" id="pet-stage" data-tauri-drag-region></div>
    <div class="pet-tool" id="pet-tool" hidden></div>
    <div class="pet-label" id="pet-label"></div>
  `;
  const stage = root.querySelector<HTMLElement>("#pet-stage")!;
  stage.appendChild(buildClawdSvg());
  return {
    stage,
    bubble: root.querySelector<HTMLElement>("#pet-bubble")!,
    toolTag: root.querySelector<HTMLElement>("#pet-tool")!,
    label: root.querySelector<HTMLElement>("#pet-label")!,
  };
}

const KNOWN_STATES = [
  "state-idle",
  "state-thinking",
  "state-working",
  "state-responding",
  "state-waiting",
  "state-sleeping",
];

function applyState(els: PetEls, session: SessionState): void {
  const cls = stateToClass(session.state);
  els.stage.classList.remove(...KNOWN_STATES);
  els.stage.classList.add(cls);

  const bubble = BUBBLE[session.state.kind];
  els.bubble.textContent = bubble;
  els.bubble.hidden = bubble === "";

  if (session.state.kind === "working" && session.state.tool) {
    els.toolTag.textContent = session.state.tool;
    els.toolTag.hidden = false;
  } else {
    els.toolTag.hidden = true;
  }

  els.label.textContent = session.project;
}

/** Open the dashboard window. In mock mode there's no backend, so log. */
async function openDashboard(): Promise<void> {
  const url = `${baseUrl()}/`;
  try {
    // In a Tauri shell a custom command would handle this; for the browser /
    // mock case just navigate or log.
    if (baseUrl()) {
      window.open(url, "_blank");
    } else {
      // eslint-disable-next-line no-console
      console.log("[clawd] open dashboard (mock)");
    }
  } catch {
    // eslint-disable-next-line no-console
    console.log("[clawd] open dashboard (fallback)", url);
  }
}

function bootstrap(): void {
  const root = document.getElementById("pet") ?? document.body;
  const els = renderShell(root as HTMLElement);
  const sessionId = getSessionId();

  // Initial placeholder state until the stream resolves our session.
  applyState(els, {
    sessionId: sessionId ?? "",
    project: "",
    model: "",
    state: { kind: "idle" },
    tokens: 0,
    updatedAt: Date.now(),
  });

  root.addEventListener("click", () => void openDashboard());

  const pick = (sessions: SessionState[]): SessionState | undefined => {
    if (sessionId) return sessions.find((s) => s.sessionId === sessionId);
    return sessions[0]; // standalone/mock: just take the first
  };

  subscribe((ev: CmServerEvent) => {
    if (ev.type !== "sessions") return;
    const mine = pick(ev.data);
    if (mine) applyState(els, mine);
  });
}

function autostart(): void {
  // Only auto-boot when a real, empty pet mount exists. Lets tests import
  // this module and call applyState without side effects.
  const mount = typeof document !== "undefined" ? document.getElementById("pet") : null;
  if (mount && mount.childElementCount === 0) bootstrap();
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    window.addEventListener("DOMContentLoaded", autostart);
  } else {
    autostart();
  }
}

export { applyState, BUBBLE, bootstrap };
