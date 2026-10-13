# A local Jev for task size: indecis trained on Forge's own record

*2026-10-13. Can Forge's typed judgments run on a model it trains itself?
First question tried: task size, the eval set with the most labels. Short
answer: yes. On identical held-out items it beats Jev by 33 points and
the untrained local model by 37, with calibrated probabilities where Jev's
confidence runs backwards.*

## What indecis is

[Bornholm/indecis](https://github.com/Bornholm/indecis) (MIT, v0.4.0,
2026-10-06) builds small decision models in Go: a pretrained ModernBERT
encoder, fully fine-tuned on labeled examples with one head per question,
calibrated on a 5% split, served behind the TypeSafe decision API
(`POST /v1/systemone`), so a client written for Jev works by changing the
URL. CPU only. The backbone is `bekko-embedding-v1-a8m`: 4 layers, width
384, 7.7M parameters outside a 256k x 384 embedding table, 202 MiB on
disk, 12 MB resident idle when served. Untrained, it answers "open"
questions by embedding similarity and its probabilities are not
calibrated; that is what the `indecis` provider on port 8117 has been.

## Setup

- Labels: `forge eval jev --record` pulled the size set from the store:
  853 landed tasks, each task's text as state, labeled from lines changed
  at landing (small at most 50, medium at most 300, large above):
  large 531, medium 271, small 51.
- Split: 80/20 by example with seed 7: 683 train (of which 34 held by
  the trainer for calibration), 170 test (large 100, medium 61, small 9).
  The test items are `test-fixture.json`, replayable with
  `forge eval jev --fixture`, so every model is scored on the same items.
- Training: `indecis train -backbone bekko-embedding-v1-a8m -schema
  schema.json -train train.jsonl -test test.jsonl -epochs 4 -max-len 512
  -seed 7`, one `score` question named `outcome` (the name Forge's eval
  and judgment steps send) over small, medium, large. 2m05s wall on 8
  cores, no GPU.
- Serving: `indecis-serve -model size=~/.local/share/indecis/models/size-e4
  -addr 127.0.0.1:8118` as the user unit `indecis-size`; the operator
  provider `[providers.indecis-size]` (runner jev, backend typesafe,
  model `size`). Since 2026-10-13 it is the `[admission] size_provider`:
  every claimed task gets a predicted size, and `size_workflows` routes
  the workflow on it (docs/EXECUTION.md, "Admission and the judgment
  band", 3). The 8117 and 8118 servers are the user units `indecis` and
  `indecis-size` (~/.config/systemd/user), enabled at login.

## Results on the 170 held-out items

| model | accuracy | note |
|---|---|---|
| always "large" | 58.8% | the majority class |
| indecis, untrained open mode (`indecis`, port 8117) | 34.1% | below the majority: it prefers "medium" |
| indecis, trained (`indecis-size`, port 8118) | **71.2%** | MAE 0.37 levels, ECE 0.09, 19 ms mean |
| Jev (TypeSafe), same fixture | 38.2% | 111 ms mean, $0.005 for the run; its 0.9-1.0 confidence decile is 22% right, its 0.2-0.3 decile 67% |
| Jev (TypeSafe), 2026-09-27 | 23.3% | 60 different items; see jev-eval-2026-09-27.md |

Calibration of the trained model (Forge's eval, confidence decile against
accuracy):

| confidence | answers | mean confidence | accuracy |
|---|---|---|---|
| 0.4-0.5 | 13 | 0.47 | 76.9% |
| 0.5-0.6 | 37 | 0.55 | 40.5% |
| 0.6-0.7 | 29 | 0.65 | 55.2% |
| 0.7-0.8 | 32 | 0.74 | 87.5% |
| 0.8-0.9 | 36 | 0.85 | 86.1% |
| 0.9-1.0 | 23 | 0.93 | 91.3% |

Confusion (rows gold, columns predicted):

| | small | medium | large |
|---|---|---|---|
| small | 0 | 4 | 5 |
| medium | 0 | 37 | 24 |
| large | 0 | 16 | 84 |

Per class: large precision 0.74 recall 0.84; medium precision 0.65 recall
0.61; small never predicted (9 test items, 42 in training). The errors are
the adjacent level, never two levels off, which the MAE of 0.37 says too.

## What this says

- The question is learnable from the task text alone, well past the
  majority baseline, on under a thousand examples and two minutes of CPU.
  The trained model's confidence is usable as a probability: the 0.9 to
  1.0 decile is 91% right, the 0.5 to 0.6 decile 41%.
- "Small" does not exist for this model. Fifty-one examples of a class the
  text rarely betrays (a one-line fix reads like a medium one) are not
  enough; a class weight, or folding small into medium for routing, is the
  likely answer. For the workflow-choice use (docs/EXECUTION.md, "Measuring
  it") the split that matters is large against the rest, where the model is
  at 0.79 F1.
- Open mode is worse than guessing on this question, which matches the
  indecis docs' own warning that open answers rank rather than decide.
  The `indecis` provider on 8117 should not be read as a Jev stand-in for
  anything but plumbing tests.

## Jev on the same items

Run the same afternoon once the TypeSafe key was in Forge's secret store
(`forge secret set TYPESAFE_API_KEY`): 38.2% on the 170, every item
answered, no errors (full report: jev-size-2026-10-13.md). Its calibration
table is the finding: accuracy falls as confidence rises, 67% in the 0.2
to 0.3 decile and 22% in the 0.9 to 1.0 decile. A probability that runs
backwards cannot be thresholded, which is what every Forge use of Jev
does (`[admission] threshold`, `[risky] threshold`). On this question the
local model is the only one of the three whose confidence means anything.

Jev stays the right tool where Forge has no labels yet (admission, the
band, the risky verdict, the miss class) and where the state is not a
task text. Each of those now records its answers, so the same recipe
applies to them once a few hundred rows exist, with the task's later fate
as the label.

## Reproduce

Training data and the fixture are derived from the store, not committed:
`forge eval jev --provider indecis --record sets.json` regenerates the
sets (853 size items at the time of writing; more as tasks land). Source
checkout and binaries: ~/Projects/indecis (bin/indecis, bin/indecis-serve;
Go 1.27 rebuilds them). The backbone is cached at
~/.cache/indecis/models/bekko-embedding-v1-a8m.
