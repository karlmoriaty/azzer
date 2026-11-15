# Azzer

**Turn a real vocal take into a release-ready track in a target voice — one that doesn't sound AI-made.**

<sub>中文版见 [README.zh-CN.md](./README.zh-CN.md)</sub>

---

## Why AI covers sound fake

It's not the timbre. Voice cloning got convincing a while ago.

What gives it away is everything *else*:

- No breath before the phrase. The singer finishes a 20-second line without inhaling.
- Every pitch lands exactly on an equal-tempered grid line. It sounds like a keyboard, not a throat.
- Every note has the same length. No rushing, no dragging.
- Long notes are dead flat — no vibrato, or vibrato that pulses like a machine.
- The room goes silent the instant the phrase ends. No space, no tail, no sense of a person in a room.

These have one thing in common: **nothing is missing that a better model could add. Something that was already there got destroyed.**

A real recording carries breath, mouth noise, micro-detuning, timing elasticity, irregular vibrato. All of it is *in the source*. Every processing stage removes a little more of it.

---

## So we work backwards

Most tools ask: how do we make generated singing sound more human?

Azzer asks: **how do we keep the human parts from being lost?**

That one question reshapes the design:

| Common approach | Azzer |
|---|---|
| A stronger model generates better vocals | Minimal processing preserves the source vocals |
| Metric: generation quality, higher is better | Metric: deviation from the source, lower is better |
| The model is the star | **The model is a supporting actor** — its only job is to not break things |
| Goal: sing better than the human | Goal: you can't tell it isn't the human |

This leads to an uncomfortable conclusion: **this is not a quality arms race.**

End-to-end generative models (the Suno class) are structurally ahead on fidelity — they hold internal stem representations and ship with a built-in mastering chain. We take a mixed, finished recording and pull it apart, and that separation loss is irreversible.

So Azzer never claims to sound better than Suno.

What it claims is different: **it preserves how this person sings.** Where they breathe. How they shape consonants. That half-beat they always drag. Those things only exist in the source — a regenerating model can never learn them.

---

## Three entry points, three different costs

| Entry | When | Path | Vocal realism | Finished feel |
|---|---|---|---|---|
| **A · Direct** | Source timbre ≈ target (singing your own song) | Skip conversion; repair + humanize + master | **Highest** | Depends on backing |
| **B · Convert** | Source is a dry or a cappella take | Separate → repair → convert → humanize | High | Depends on backing |
| **C · Cover** | Source is a released track (with backing) | Integrated separate → repair → convert → humanize | Medium (separation loss is irreversible) | **Highest** (inherits the original arrangement) |

Note that A and C move in **opposite** directions: C has the weakest vocal realism but potentially the best finished feel, because the backing is the original commercial arrangement.

These are two independent axes. Collapsing them into a single "how good is it" score would be a lie.

---

## How it works

A take goes through 16 nodes:

```
N01 normalize → N02 grade ─┬─→ N03 separate → N04 merge ─┐
                           │                             ├─→ N06 pre → N07 convert → N08 gate
                           └─→ N05 f0 ──────────────────┘
                                                          ↓
      N09 repair → N10 rebreath → N11 humanize → N12 space → N13 layers → N14 mix
                                                          ↓
                                        N15 master → N16 37-metric evaluation
```

Every node has five things fixed in its contract: **inputs / outputs / contract assertions / fallback / idempotency key**.

The idempotency key is `sha256(upstream artifact + params + engine version + adapter_hash)`. Change one humanize parameter and only the last five nodes rerun — **30 seconds** instead of 11 minutes from scratch. That is where all the tuning speed comes from.

Six layers, dependencies pointing one way:

```
L1  Experience host   (Tauri + React)
L2  Orchestration     (axum, single entry)
L3  Planning          (Song IR, single authority)
L4  Scheduling        (node DAG — forbidden to `use ort` directly)
L5  Engines           (DSP / ONNX inference)
L6  Assets            (content-addressed storage)
```

`scripts/check-layering.cjs` enforces that line, because "L4 quietly imports the inference SDK" compiles just fine.

---

## Models: not bundled, not downloaded, not hosted

This is a hard constraint, not a preference.

The software ships **zero model weights** — not in the installer, not in this repo, not in build artifacts. Startup never checks whether a "required model" exists; a missing model is a **normal state**. There is no auto-download and no model registry URL.

You declare paths yourself in `models.json` at the run root:

```json
{
  "models": [
    { "id": "rmvpe", "role": "f0",
      "path": "D:/models/rmvpe.onnx", "engine": "onnx-local" }
  ]
}
```

No such file? Fine. Entry A still runs on the built-in algorithms.

**Why**: every open-source voice model available is incomplete in some way. Baking one into a product bakes its defects into the product. And models get replaced; the software shouldn't.

The cost is that you have to supply models yourself. We accept that cost.

---

## We don't compete with Suno

This table lives in an internal document that forbids marketing language:

| Dimension | Suno (checked 2026-11) | Azzer | Verdict |
|---|---|---|---|
| Voice cloning | Cloud, verification required, unavailable in some regions | Local, model is yours, portable | **Win** (ownership) |
| Preserving the performance | Cover re-performs; phrasing changes | Keeps the source's pitch, timing, breaths | **Win** (structural) |
| Local editing | Section-level regeneration | Note-level patches | **Win** |
| Offline / data ownership | Cloud | Fully local | **Win** |
| Stem separation | Internal stem representations | Signal-level separation | Loss |
| Overall fidelity / mastering | End-to-end generation + built-in mastering | Rebuilt from separated dry vocals + our own chain | Loss |
| Mandarin articulation | Strong on its main languages | Inherits the source; untested | Loss (untested) |
| Time to output | <1 min for 2 tracks | 2–4 min | Loss (order of magnitude) |

Four wins, three losses, one untested. That's the real position.

---

## How we know it works

**There is exactly one acceptance criterion**: a human detection test.

20 triads (real original / Azzer output / Suno output under the same conditions, shuffled) × 30 listeners = 600 judgments.

Pass line:

- Azzer detection rate **≤ 40%**
- and ≤ Suno's detection rate × 1.1

The random baseline for a 3-way choice is 33.3%.

The other 37 metrics (loudness, true peak, LRA, HNR, SECS…) are **explanatory variables** — used to locate which stage is dragging when a number is bad, not to comfort ourselves.

**This test has never been run.** Building the triads requires entry C (separate a finished track, convert, remix), and entry C doesn't exist yet.

Which means there is currently no evidence that the target is even reachable. That is the most honest statement about this project.

---

## Where it stands

**Six of sixteen nodes implemented**: N01 normalize / N02 grade / N05 f0 / N09 repair / N15 master / N16 evaluation.

Of the three entries, **A is wired end-to-end**.

Measured on a 4.4-minute real vocal take (44.1 kHz / 16-bit):

| Item | Result |
|---|---|
| End-to-end time | 2–4 min (depending on whether a model is enabled) |
| Master output | **−14.00 LUFS / −1.10 dBTP**, on target in one pass |
| Evaluation coverage | 10 of 37 metrics measured; 27 explicitly state what they'd need |
| Rust tests | **142** |
| Numerical cross-check | Independent Python implementation vs Rust: 7/7 pass, max deviation **7.6e-07** |

Pitch extraction has two tiers:

| Metric | Built-in (zero model) | Optional RMVPE |
|---|---|---|
| Median f0 | 108.5 Hz | **173.5 Hz** |
| Octave error rate | 8.1% | **4.6%** |
| Deviation from Python reference | — | **1.2%** |

RMVPE is a pure-Rust port (htk mel filterbank + STFT + decoding). No Python at runtime.

**Not built yet**: N03/N04 separation, N06–N08 conversion, N10–N14 humanization, first successful Tauri build, waveform editor.

Eight issues track these gaps. Each carries measured data, parameters, reference implementation locations and acceptance criteria — **a new contributor doesn't start from zero.**

---

## Running it

Requires Rust 1.92+ and a working C++ linker.

```powershell
# Build
node azzer/scripts/cargo-run.cjs build

# Test (142)
node azzer/scripts/cargo-run.cjs test

# Run the full A entry on any vocal take and see what this source can produce
node azzer/scripts/check-source.cjs D:\path\to\vocal.wav
```

The last one tells you how many of the 37 metrics are computable, which are inapplicable, and which HumanizeScore sub-item is missing. Use it on real material instead of typing command lines by hand.

For GPU: `azzer/scripts/fetch-cuda.ps1` unpacks CUDA and cuDNN DLLs into `native/` and prepends it to the process PATH at runtime. No driver install, no registry edits, no permanent environment variables.

---

## What's in here

```
azzer/            Rust workspace (7,645 lines across 33 files)
├── crates/
│   ├── azzer-core/      16-node contracts, idempotency keys, CAS, model registry
│   ├── azzer-dsp/       decode / resample / STFT / f0 / repair / master / percept / 37 metrics
│   ├── azzer-pipeline/  node implementations and the A-entry CLI
│   ├── azzer-infer/     ONNX Runtime + RMVPE
│   └── azzer-api/       axum + token auth
├── apps/web/            React frontend (Vite + hand-rolled Canvas waveform)
├── src-tauri/           Tauri shell (code ready, never compiled)
└── scripts/             build, checks, e2e, source inspection

docs/             Design documents ([index](./docs/README.md)) — product plan, tech stack, six detailed references
```

**No model weights.** Get them under their own licenses and declare the paths in `models.json`.

---

## Contributing

What this project lacks is not code. It lacks two things:

1. **One real "vocal + backing" mix** (30 s to 1 min, clean rights) — right now there's only an isolated vocal track, so separation quality can't be validated
2. **30 listeners** — the only acceptance criterion needs human judgment; there is no technical substitute for it

Everything else is filed as an issue, tagged by milestone, with technical detail attached.

If "keep the human parts intact" is a problem you find interesting, pick any issue and start.

---

## One sentence

**Azzer doesn't generate a voice. It refuses to lose one.**

One aims for better. The other aims for less loss.
