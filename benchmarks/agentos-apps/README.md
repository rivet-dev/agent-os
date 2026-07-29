# agentOS Apps load test

Start `examples/apps-hello-world`, then run:

```sh
pnpm --filter @rivet-dev/agentos-benchmark-apps load
```

The bounded driver reports p50, p89, p95, and p99 latency for all, cold, and
warm requests, plus queue delay, replica distribution, throughput, and status
counts.

The defaults run 16 concurrent clients for 10 seconds, with hard limits of
100,000 requests, 100,000 latency samples, 1 MiB per response, 1,024 replica
series, and 10 seconds per request. Configure them with:

| Variable | Default |
| --- | ---: |
| `LOAD_TEST_URL` | `http://127.0.0.1:3000/apps/hello-world` |
| `LOAD_TEST_CONCURRENCY` | `16` |
| `LOAD_TEST_DURATION_SECONDS` | `10` |
| `LOAD_TEST_TIMEOUT_MS` | `10000` |
| `LOAD_TEST_MAX_REQUESTS` | `100000` |
| `LOAD_TEST_MAX_SAMPLES` | `100000` |
| `LOAD_TEST_MAX_RESPONSE_BYTES` | `1048576` |
| `LOAD_TEST_MAX_REPLICA_SERIES` | `1024` |

Optional `LOAD_TEST_MAX_P95_MS` and `LOAD_TEST_MIN_SUCCESS_RATE` (from `0` to
`1`) turn the run into a failing performance gate. A run can take up to one
request timeout beyond its configured duration while the final in-flight
requests finish.
