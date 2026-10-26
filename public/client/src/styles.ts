// The look: browser chrome that feels native in the system's own face, so an embed never pulls fonts into the page
// that hosts it; cool neutral surfaces in raven ink, one iris accent for focus, and the session capsule -- live,
// view-only or disconnected -- as the one thing that says this browser runs somewhere else. Every colour is a custom
// property an embedder can set on the element.

const tokens = `
  --korvus-font: system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans", "Noto Sans KR", sans-serif;
  --korvus-ink: #1d2330;
  --korvus-ink-soft: #5b6475;
  --korvus-strip: #e3e8ee;
  --korvus-bar: #f6f8fa;
  --korvus-tab: #ffffff;
  --korvus-line: #c7cfd9;
  --korvus-field: #ffffff;
  --korvus-accent: #4a3fd0;
  --korvus-live: #1d8a5a;
  --korvus-view-only: #9a6410;
  --korvus-off: #a9382c;
  --korvus-page: #ffffff;
  --korvus-scrim: rgb(29 35 48 / 0.32);
`;

const dark = `
  --korvus-ink: #e2e6ed;
  --korvus-ink-soft: #9aa3b2;
  --korvus-strip: #181c24;
  --korvus-bar: #20252f;
  --korvus-tab: #2a303c;
  --korvus-line: #39414f;
  --korvus-field: #171b22;
  --korvus-accent: #a59cff;
  --korvus-live: #4cc58e;
  --korvus-view-only: #e0a447;
  --korvus-off: #f0806f;
  --korvus-scrim: rgb(0 0 0 / 0.45);
`;

export const viewStyles = `
  :host { ${tokens} display: block; position: relative; overflow: hidden; background: var(--korvus-page);
    font: 13px/1.4 var(--korvus-font); color: var(--korvus-ink); contain: strict; }
  @media (prefers-color-scheme: dark) { :host { ${dark} } }
  .stage { position: absolute; inset: 0; }
  canvas { position: absolute; inset: 0; width: 100%; height: 100%; display: block; touch-action: none; outline: none; }
  .keys { position: absolute; left: 0; top: 0; width: 1px; height: 1px; padding: 0; border: 0; margin: 0;
    opacity: 0; resize: none; overflow: hidden; caret-color: transparent; font-size: 16px; }
  .note { position: absolute; inset: 0; display: grid; place-items: center; padding: 24px; text-align: center;
    color: var(--korvus-ink-soft); background: var(--korvus-bar); }
  .note[hidden], .dialog-layer[hidden], [hidden] { display: none !important; }
  .dialog-layer { position: absolute; inset: 0; background: var(--korvus-scrim); display: flex; justify-content: center;
    align-items: flex-start; padding-top: 12px; }
  .dialog { width: min(440px, calc(100% - 32px)); background: var(--korvus-tab); color: var(--korvus-ink);
    border: 1px solid var(--korvus-line); border-radius: 10px; padding: 18px 20px 14px;
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.22); }
  .from { margin: 0 0 6px; font-weight: 600; }
  .message { margin: 0 0 14px; white-space: pre-wrap; overflow-wrap: anywhere; max-height: 40vh; overflow: auto; }
  .prompt { width: 100%; box-sizing: border-box; font: inherit; color: inherit; background: var(--korvus-field);
    border: 1px solid var(--korvus-line); border-radius: 6px; padding: 6px 8px; margin-bottom: 14px; }
  .buttons { display: flex; justify-content: flex-end; gap: 8px; }
  button { font: inherit; color: inherit; background: var(--korvus-bar); border: 1px solid var(--korvus-line);
    border-radius: 6px; padding: 5px 14px; cursor: pointer; }
  .ok { background: var(--korvus-accent); border-color: var(--korvus-accent); color: #fff; }
  button:focus-visible, .prompt:focus-visible { outline: 2px solid var(--korvus-accent); outline-offset: 2px; }
`;

export const browserStyles = `
  :host { ${tokens} display: flex; flex-direction: column; min-height: 240px; background: var(--korvus-strip);
    font: 13px/1.4 var(--korvus-font); color: var(--korvus-ink); position: relative; overflow: hidden; }
  @media (prefers-color-scheme: dark) { :host { ${dark} } }
  [hidden] { display: none !important; }
  button { font: inherit; color: inherit; background: none; border: 0; cursor: pointer; }
  button:focus-visible, input:focus-visible { outline: 2px solid var(--korvus-accent); outline-offset: -2px; }
  svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round;
    stroke-linejoin: round; }

  .strip { display: flex; align-items: flex-end; gap: 2px; padding: 6px 8px 0; min-height: 36px; box-sizing: border-box; }
  .tabs { display: flex; gap: 2px; min-width: 0; flex: 1 1 auto; overflow-x: auto; scrollbar-width: none; }
  .tab { display: flex; align-items: center; gap: 6px; flex: 0 1 220px; min-width: 72px; height: 30px;
    padding: 0 6px 0 12px; border-radius: 8px 8px 0 0; color: var(--korvus-ink-soft); position: relative; }
  .tab:hover { background: color-mix(in srgb, var(--korvus-tab) 55%, transparent); }
  .tab[aria-selected="true"] { background: var(--korvus-bar); color: var(--korvus-ink); }
  .tab .title { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; text-align: left; }
  .tab .spin { width: 10px; height: 10px; border-radius: 50%; border: 2px solid var(--korvus-line);
    border-top-color: var(--korvus-accent); animation: spin .8s linear infinite; flex: none; }
  .tab .x { display: grid; place-items: center; width: 20px; height: 20px; border-radius: 5px; flex: none; }
  .tab .x:hover { background: var(--korvus-strip); }
  .tab .x svg { width: 12px; height: 12px; }
  .new { display: grid; place-items: center; width: 28px; height: 28px; margin-bottom: 1px; border-radius: 6px;
    color: var(--korvus-ink-soft); flex: none; }
  .new:hover { background: var(--korvus-tab); color: var(--korvus-ink); }

  .capsule { display: flex; align-items: center; gap: 6px; margin: 0 0 5px 8px; padding: 3px 10px 3px 8px;
    border-radius: 999px; border: 1px solid var(--korvus-line); background: var(--korvus-bar); white-space: nowrap;
    color: var(--korvus-ink-soft); flex: none; font-size: 12px; }
  .capsule .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--korvus-live); }
  .capsule[data-state="view-only"] .dot { background: var(--korvus-view-only); }
  .capsule[data-state="off"] .dot { background: var(--korvus-off); }
  .capsule[data-state="connecting"] .dot { background: var(--korvus-line); }
  .capsule .persona { color: var(--korvus-ink); font-weight: 600; }
  .capsule button { text-decoration: underline; padding: 0; color: var(--korvus-ink); }

  .bar { display: flex; align-items: center; gap: 2px; padding: 5px 8px; background: var(--korvus-bar);
    border-bottom: 1px solid var(--korvus-line); }
  .nav { display: grid; place-items: center; width: 30px; height: 30px; border-radius: 50%; color: var(--korvus-ink); }
  .nav:hover:not(:disabled) { background: var(--korvus-strip); }
  .nav:disabled { color: var(--korvus-line); cursor: default; }
  .address { flex: 1; min-width: 0; height: 30px; margin-left: 6px; padding: 0 14px; box-sizing: border-box;
    border-radius: 999px; border: 1px solid transparent; background: var(--korvus-strip); color: var(--korvus-ink);
    font: inherit; font-size: 14px; }
  .address:focus { background: var(--korvus-field); border-color: var(--korvus-accent); outline: none; }

  .pages { flex: 1; position: relative; background: var(--korvus-page); min-height: 0; }
  .pages > korvus-view { position: absolute; inset: 0; }
  .empty { position: absolute; inset: 0; display: grid; place-items: center; color: var(--korvus-ink-soft);
    background: var(--korvus-bar); padding: 24px; text-align: center; }

  .windows { position: absolute; inset: 0; pointer-events: none; }
  .pages[data-dialog] .windows { opacity: 0.3; }
  .pages[data-dialog] .window { pointer-events: none; }
  .window { position: absolute; pointer-events: auto; display: flex; flex-direction: column; background: var(--korvus-bar);
    border: 1px solid var(--korvus-line); border-radius: 10px; overflow: hidden; resize: both;
    box-shadow: 0 18px 48px rgb(0 0 0 / 0.28); min-width: 280px; min-height: 200px; }
  .window header { display: flex; align-items: center; gap: 8px; padding: 0 6px 0 12px; height: 34px; cursor: move;
    user-select: none; border-bottom: 1px solid var(--korvus-line); }
  .window header .title { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  .window header .host { color: var(--korvus-ink-soft); }
  .window korvus-view { flex: 1; min-height: 0; }
  .window .x { display: grid; place-items: center; width: 24px; height: 24px; border-radius: 6px; }
  .window .x:hover { background: var(--korvus-strip); }

  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .tab .spin { animation: none; border-top-color: var(--korvus-line); } }
  @media (max-width: 560px) {
    .capsule .label { display: none; }
    .tab { flex-basis: 140px; }
  }
`;
