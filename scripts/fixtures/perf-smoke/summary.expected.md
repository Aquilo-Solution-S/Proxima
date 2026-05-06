# Perf session: fixtures

Duration: 30.0s

## IPC summary

| cmd | calls | p50 ms | p95 ms | p95 req | p95 resp |
|---|---:|---:|---:|---:|---:|
| query | 2 | 400 | 418 | 50 B | 117.1 KiB |
| repos_list | 1 | 15 | 15 | 10 B | 2.0 KiB |

## MCP summary

| method route | calls | p50 ms | p95 ms | p95 resp | statuses |
|---|---:|---:|---:|---:|---|
| POST /mcp | 2 | 135 | 149 | 895 B | 200:2 |

## Frontend timings

| kind | name | count | p50 ms | p95 ms |
|---|---|---:|---:|---:|
| render | atlas_first_paint | 1 | 580.0 | 580.0 |
| selector | atlas_projection | 2 | 38.6 | 41.7 |

## Wasted IPC fields

Per command: which fields the FE actually read.

(Wasted-set computation requires `bindings.ts` introspection — for now we list accessed paths only; cross-reference manually.)

### query

Accessed: 3 paths

```
goals.[].id
goals.[].title
meta.count
```

## Top PG statements

| total ms | calls | mean ms | query |
|---:|---:|---:|---|
| 1201 | 40 | 30.0 | `SELECT * FROM goals WHERE owner_org_id = $1` |
| 600 | 12 | 50.0 | `SELECT * FROM memories` |

## Top engine spans

| dur ms | name |
|---:|---|
| 420.0 | snapshot_assemble |
| 280.0 | goal_query |
