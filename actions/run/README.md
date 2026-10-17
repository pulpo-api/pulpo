# Pulpo run · GitHub Action

Runs a Pulpo collection in CI: every request with its scripts and tests, the same way the
app and `pulpo run` do. It writes JUnit, HTML and Markdown reports, adds the summary to the
job page and comments it on the pull request (one comment, updated on every push).

```yaml
jobs:
  api:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write # for the comment
    steps:
      - uses: actions/checkout@v4
      - uses: pulpo-api/pulpo/actions/run@main
        with:
          workspace: api-tests
          environment: Staging
          data: api-tests/users.csv
          args: --retries 2
        env:
          PULPO_SECRET_TOKEN: ${{ secrets.STAGING_TOKEN }}
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: pulpo-reports
          path: pulpo-reports
```

| Input | Default | |
|---|---|---|
| `workspace` | `.` | Folder with `pulpo.yaml` |
| `target` | everything | Request or folder to run |
| `environment` | none | Environment name or file |
| `data` | none | CSV or JSON, one row per iteration |
| `iterations` | rows or 1 | How many times to run |
| `args` | | More options: `--retries 2 --bail --delay 100` |
| `reports` | `pulpo-reports` | Where `junit.xml`, `report.html` and `summary.md` go |
| `comment` | `true` | Comment the summary on the pull request |
| `pulpo` | | A pulpo binary to use instead of installing one |
| `version` | `main` | Branch of `pulpo-api/pulpo` to install the CLI from |

Secrets go in `PULPO_SECRET_<NAME>` environment variables, never in the workspace.
The output `passed` is `"true"` when everything passed; the step fails otherwise.
