# Perf reducer fixtures

## Reducer smoke

`node scripts/perf-smoke.mjs` runs the summary reducer against committed
fixtures and compares to a golden `summary.expected.md`. Use `--regen`
after intentional reducer changes.
