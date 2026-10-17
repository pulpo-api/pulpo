# Runner check

A small workspace that `pulpo run` and the GitHub Action are tested with in CI, against
`tests/codegen/echo.py` (it answers with the request it got):

```bash
python3 tests/codegen/echo.py 8765 &
pulpo run -w tests/runner/workspace -e Local --data tests/runner/users.csv
```

Each data row is one iteration; the first request keeps a snapshot per user.
