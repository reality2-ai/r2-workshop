# AGENTS.md — Orientation for AI Agents working in `r2-workshop`

This file is the entry point for any AI agent (Claude Code, Codex, Cursor, …) operating in this
repository. Read this first, then [`README.md`](README.md), then `RESUME.md` for running state.

> **One-paragraph orientation:** `r2-workshop` (renamed from `r2-rocker`, 2026-05-24) is a wireless
> sensor-mesh application for workshop / lab environments — vibration, temperature, pressure, strain,
> with edge anomaly detection. It is an **R2 downstream app**, not a core layer: it composes
> `../r2-core` and conforms to the canonical specs at `../r2-specifications`. It also carries the
> precedent for the **wasm-TG-hive-in-browser** and **UX-plugin-that-owns-its-own-hive** patterns.

## 1. Status

**STABLE maintenance-mode** (held per Roy). This repo is not the active R2 development front. Do not
start net-new feature work here without an explicit directive. **Sole writer** is tracked in
`RESUME.md` (one writer per repo, never two at once — reconcile before handing back).

## 2. What binds you

- **Authority chain:** `r2-specifications → r2-core → r2-hive / downstream`. This repo is downstream.
  It does **not** redefine the plugin / sentant / ensemble / trust model — it consumes them. If your
  work touches those, read the relevant spec under `../r2-specifications/specs/r2-core/` and cite it.
- **This app uses *some* of R2's goodness, tempered by context — it is not, and need not be, a full
  peer-mesh TN node.** R2 is a toolkit you apply with judgment, not a conformance box every deployment
  must tick. Here the context is a workshop/lab sensor deployment, and a **hub-and-spoke** topology — a
  laptop acting as the central data gatherer and controller of the sensors — is the *sensible* shape,
  not a failure to reach full peer mesh. So its deviations from canon are often **deliberate
  context-fit**, not gaps to close. Two consequences for you:
  - Do **not** reflexively "bring it to full conformance." First understand which R2 pieces it uses and
    *why* the tempering fits the context; change topology only on an explicit, reasoned directive.
  - Do **not** treat this repo's TN code as a reference implementation of canon — for the pieces it
    *does* use, `../r2-specifications` is the source of truth (e.g. `R2-TRUST` device/TG identity,
    `R2-BEACON` discovery). Flag divergences against the specs; judge each as either intended
    context-fit or a real gap, don't assume.

## 3. Working principles (inherited from `r2-specifications/AGENTS.md`)

- **Conjecture-and-refutation** — try to refute every decision; "found nothing against it" is neutral.
- **Occam's razor** — simplest implementation that meets the requirement wins.
- **Disagree with the operator when they are wrong**, politely.
- **Citation discipline** — read/grep/fetch before citing a path, spec section, or datasheet.
- **Cheaper honest move** — downgrade an overclaim rather than overstate.
- **Autonomy stop** — STOP before a hard-to-reverse action and surface it.

The full treatment lives at `../r2-specifications/AGENTS.md` §2.
